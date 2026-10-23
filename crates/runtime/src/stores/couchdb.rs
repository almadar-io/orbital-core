//! The CouchDB row store: a kernel store driver over the REST API, reached through the raw http port
//! ([`Transport`]). It is the Rust twin of `@almadar/db/couchdb` and keeps its document layout, so either side reads
//! the other's rows:
//!
//! - one database per entity type, named like the Postgres table (`TimeEntry` -> `time_entries`);
//! - a row is a schemaless document whose `_id` is the row's `id`; a read strips `_id` and `_rev`;
//! - queries are Mango `_find` selectors sorted on `_id`; a query outside [`query_capabilities`] is declined and the
//!   kernel reads rows instead;
//! - CouchDB has no multi-document transactions, so a write is conditioned on the `_rev` it read: a guard is tested in
//!   process on the pre-image, a write that loses the race re-reads and re-tests, and a batch cannot be atomic.

use std::collections::HashSet;
use std::sync::Arc;

use orbital_core_base::codec::{base64_encode, encode_uri_component};
use orbital_core_base::row_set::{RowCompareOp, RowPredicate, RowQuery, RowTotalMode};
use orbital_core_base::value::Value;
use parking_lot::{Mutex, RwLock};
use serde_json::{json, Map, Value as Json};

use crate::entity_source::EntitySource;
use crate::firestore::{from_json, to_json, total_from_window};
use crate::http_service::{HttpRequest, Transport};
use crate::pg_rows::{mint_id, storage_name_for, Collections};
use crate::row_query::{
    query_unsupported_reason, GuardCapabilities, OperatorValueKinds, QueryCapabilities,
    RowCombinator, RowIdPage, RowLeafKind, RowOperator, RowOrderableFields, RowPage, RowValueKind,
    RowWriteBatch, RowWriteFailureMode, RowWriteOp, RowWriteOutcome, WriteCapabilities,
};

/// A write that loses a `_rev` race re-reads and tries again this many times.
const WRITE_ATTEMPTS: usize = 5;

/// What Mango expresses with the shared two-valued semantics. Ordering comparisons are declared for numbers and
/// booleans only, and the selector pins the value's type (`$type`) because Mango orders across types; strings are left
/// out (Mango orders them by ICU collation, not binary order) and so is `contains`. Only `id` can be ordered (a sort
/// needs an index and skips documents lacking the field; `_id` is the one field every document has).
pub fn query_capabilities() -> QueryCapabilities {
    let kinds = vec![
        RowValueKind::Number,
        RowValueKind::Boolean,
        RowValueKind::Null,
    ];
    QueryCapabilities {
        leaves: vec![
            RowLeafKind::Compare,
            RowLeafKind::Member,
            RowLeafKind::HasElement,
            RowLeafKind::IsNull,
        ],
        combinators: vec![RowCombinator::All, RowCombinator::Any, RowCombinator::Not],
        compare_ops: all_compare_ops(),
        operator_value_kinds: Some(
            [
                RowOperator::Lt,
                RowOperator::Le,
                RowOperator::Gt,
                RowOperator::Ge,
            ]
            .into_iter()
            .map(|operator| OperatorValueKinds {
                operator,
                value_kinds: kinds.clone(),
            })
            .collect(),
        ),
        orderable_fields: RowOrderableFields::Listed {
            fields: vec!["id".to_string()],
        },
        window: true,
        exact_total: true,
        ids_only: Some(true),
        nearest: None,
    }
}

fn all_compare_ops() -> Vec<RowCompareOp> {
    vec![
        RowCompareOp::Eq,
        RowCompareOp::Ne,
        RowCompareOp::Lt,
        RowCompareOp::Le,
        RowCompareOp::Gt,
        RowCompareOp::Ge,
    ]
}

/// CouchDB cannot make a batch atomic: a plain run stops at the first failure because it is sequential writes, each
/// guarded by the `_rev` it read. A guard is tested in process, so every predicate form is accepted.
pub fn write_capabilities() -> WriteCapabilities {
    WriteCapabilities {
        guards: GuardCapabilities {
            leaves: vec![
                RowLeafKind::Compare,
                RowLeafKind::Member,
                RowLeafKind::HasElement,
                RowLeafKind::Contains,
                RowLeafKind::IsNull,
            ],
            combinators: vec![RowCombinator::All, RowCombinator::Any, RowCombinator::Not],
            compare_ops: all_compare_ops(),
        },
        conditional: true,
        returns_row: true,
        stop_runs: true,
        atomic_batch: false,
        rekey: None,
        commit_rows: None,
    }
}

// ---- Mango (`couchdb/rows.ts`) ----

fn match_none() -> Json {
    json!({ "_id": { "$exists": false } })
}

fn match_all() -> Json {
    json!({ "_id": { "$exists": true } })
}

fn scalar(value: &Value) -> Option<Json> {
    match value {
        Value::String(_) | Value::Number(_) | Value::Bool(_) => Some(to_json(value)),
        _ => None,
    }
}

fn ordering_operator(op: RowCompareOp) -> &'static str {
    match op {
        RowCompareOp::Lt => "$lt",
        RowCompareOp::Le => "$lte",
        RowCompareOp::Gt => "$gt",
        _ => "$gte",
    }
}

fn complement(op: RowCompareOp) -> RowCompareOp {
    match op {
        RowCompareOp::Eq => RowCompareOp::Ne,
        RowCompareOp::Ne => RowCompareOp::Eq,
        RowCompareOp::Lt => RowCompareOp::Ge,
        RowCompareOp::Le => RowCompareOp::Gt,
        RowCompareOp::Gt => RowCompareOp::Le,
        RowCompareOp::Ge => RowCompareOp::Lt,
    }
}

fn type_name(scalar: &Json) -> &'static str {
    if scalar.is_number() {
        "number"
    } else {
        "boolean"
    }
}

fn present_scalars(values: &[Value]) -> Vec<Json> {
    values
        .iter()
        .filter(|v| !matches!(v, Value::Null))
        .filter_map(scalar)
        .collect()
}

/// `not` pushed to the leaves: Mango's `$not` is false for a document lacking the field, but the shared two-valued
/// semantics make `not(p > 3)` true for a missing `p`. An ordering leaf is negated as "missing, or another type, or on
/// the other side"; member and hasElement likewise lead with "missing".
fn mango_not(predicate: &RowPredicate) -> Result<Json, &'static str> {
    match predicate {
        RowPredicate::Const { value } => mango_predicate(&RowPredicate::Const { value: !value }),
        RowPredicate::Not { of } => mango_predicate(of),
        RowPredicate::All { of } => mango_predicate(&RowPredicate::Any {
            of: of
                .iter()
                .map(|p| RowPredicate::Not {
                    of: Box::new(p.clone()),
                })
                .collect(),
        }),
        RowPredicate::Any { of } => mango_predicate(&RowPredicate::All {
            of: of
                .iter()
                .map(|p| RowPredicate::Not {
                    of: Box::new(p.clone()),
                })
                .collect(),
        }),
        RowPredicate::IsNull { field, is_null } => mango_predicate(&RowPredicate::IsNull {
            field: field.clone(),
            is_null: !is_null,
        }),
        RowPredicate::Compare { field, op, value } => {
            if matches!(op, RowCompareOp::Eq | RowCompareOp::Ne) {
                return mango_predicate(&RowPredicate::Compare {
                    field: field.clone(),
                    op: complement(*op),
                    value: value.clone(),
                });
            }
            let Some(scalar) = (if matches!(value, Value::Null) {
                None
            } else {
                scalar(value)
            }) else {
                return Ok(match_all());
            };
            if scalar.is_string() {
                return Err("value-kind-unsupported");
            }
            let ty = type_name(&scalar);
            Ok(json!({ "$or": [
                { field: { "$exists": false } },
                { field: { "$not": { "$type": ty } } },
                { field: { ordering_operator(complement(*op)): scalar, "$type": ty } },
            ] }))
        }
        RowPredicate::Member { field, values } => {
            let values = present_scalars(values);
            Ok(if values.is_empty() {
                match_all()
            } else {
                json!({ "$or": [{ field: { "$exists": false } }, { field: { "$nin": values } }] })
            })
        }
        RowPredicate::HasElement { field, value } => Ok(match scalar(value) {
            None => match_all(),
            Some(scalar) => {
                json!({ "$or": [{ field: { "$exists": false } }, { field: { "$not": { "$elemMatch": { "$eq": scalar } } } }] })
            }
        }),
        RowPredicate::Contains { .. } => Err("leaf-unsupported"),
    }
}

fn mango_predicate(predicate: &RowPredicate) -> Result<Json, &'static str> {
    match predicate {
        RowPredicate::Const { value } => Ok(if *value { match_all() } else { match_none() }),
        RowPredicate::Compare { field, op, value } => {
            if matches!(value, Value::Null) {
                return Ok(if *op == RowCompareOp::Ne {
                    json!({ "$and": [{ field: { "$exists": true } }, { field: { "$ne": null } }] })
                } else {
                    match_none()
                });
            }
            let Some(scalar) = scalar(value) else {
                return Ok(if *op == RowCompareOp::Ne {
                    match_all()
                } else {
                    match_none()
                });
            };
            match op {
                RowCompareOp::Eq => Ok(json!({ field: { "$eq": scalar } })),
                RowCompareOp::Ne => Ok(
                    json!({ "$or": [{ field: { "$exists": false } }, { field: { "$ne": scalar } }] }),
                ),
                range => {
                    if scalar.is_string() {
                        return Err("value-kind-unsupported");
                    }
                    Ok(
                        json!({ field: { ordering_operator(*range): scalar, "$type": type_name(&scalar) } }),
                    )
                }
            }
        }
        RowPredicate::Member { field, values } => {
            let values = present_scalars(values);
            Ok(if values.is_empty() {
                match_none()
            } else {
                json!({ field: { "$in": values } })
            })
        }
        RowPredicate::HasElement { field, value } => Ok(match scalar(value) {
            None => match_none(),
            Some(scalar) => json!({ field: { "$elemMatch": { "$eq": scalar } } }),
        }),
        RowPredicate::IsNull { field, is_null } => Ok(if *is_null {
            json!({ "$or": [{ field: { "$exists": false } }, { field: { "$eq": null } }] })
        } else {
            json!({ "$and": [{ field: { "$exists": true } }, { field: { "$ne": null } }] })
        }),
        RowPredicate::All { of } => {
            if of.is_empty() {
                return Ok(match_all());
            }
            Ok(json!({ "$and": of.iter().map(mango_predicate).collect::<Result<Vec<_>, _>>()? }))
        }
        RowPredicate::Any { of } => {
            if of.is_empty() {
                return Ok(match_none());
            }
            Ok(json!({ "$or": of.iter().map(mango_predicate).collect::<Result<Vec<_>, _>>()? }))
        }
        RowPredicate::Not { of } => mango_not(of),
        RowPredicate::Contains { .. } => Err("leaf-unsupported"),
    }
}

/// A `RowQuery` as a Mango selector and an `_id` sort; `None` for a query outside [`query_capabilities`].
pub fn mango_query_for(query: &RowQuery) -> Option<(Json, Json)> {
    if query_unsupported_reason(&query_capabilities(), query).is_some() {
        return None;
    }
    let mut parts = vec![json!({ "_id": { "$gt": null } })];
    for predicate in [&query.policy, &query.filter].into_iter().flatten() {
        parts.push(mango_predicate(predicate).ok()?);
    }
    let direction = match query.order.as_ref().map(|o| o.direction) {
        Some(orbital_core_base::row_set::RowDirection::Desc) => "desc",
        _ => "asc",
    };
    Some((json!({ "$and": parts }), json!([{ "_id": direction }])))
}

// ---- the store ----

/// A CouchDB server reached over `http`.
pub struct CouchRows {
    base: String,
    authorization: Option<String>,
    transport: Arc<dyn Transport>,
    ensured: Mutex<HashSet<String>>,
    page_size: usize,
    collections: Collections,
}

struct Reply {
    status: u16,
    body: Json,
}

fn conflict(reply: &Reply) -> bool {
    reply.status == 409
}

fn doc_body(row: &Value, id: &str) -> Map<String, Json> {
    let mut doc = match to_json(row) {
        Json::Object(map) => map,
        _ => Map::new(),
    };
    doc.insert("id".to_string(), Json::String(id.to_string()));
    doc.insert("_id".to_string(), Json::String(id.to_string()));
    doc
}

/// A document as the row it stores: `_id` and `_rev` stripped.
fn doc_to_row(doc: &Json) -> Value {
    let mut map = doc.as_object().cloned().unwrap_or_default();
    map.remove("_id");
    map.remove("_rev");
    from_json(&Json::Object(map))
}

fn rev_of(doc: &Json) -> String {
    doc.get("_rev")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_string()
}

fn id_of(value: &Value) -> Option<String> {
    value
        .as_object()
        .and_then(|o| o.get("id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl CouchRows {
    /// `url` is the server (`http://admin:pw@localhost:5984`); credentials in it become a Basic `Authorization`.
    pub fn new(url: &str, transport: Arc<dyn Transport>) -> Result<Self, String> {
        let parsed = url::Url::parse(url).map_err(|e| format!("COUCHDB_URL is not a URL: {e}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(format!(
                "COUCHDB_URL must be http:// or https://, not {}://",
                parsed.scheme()
            ));
        }
        let decode = |text: &str| {
            orbital_core_base::codec::decode_uri_component(text)
                .unwrap_or_else(|_| text.to_string())
        };
        let authorization = (!parsed.username().is_empty()).then(|| {
            let credentials = format!(
                "{}:{}",
                decode(parsed.username()),
                parsed.password().map(decode).unwrap_or_default()
            );
            format!("Basic {}", base64_encode(credentials.as_bytes()))
        });
        let mut base = parsed.clone();
        let _ = base.set_username("");
        let _ = base.set_password(None);
        Ok(Self {
            base: base.as_str().trim_end_matches('/').to_string(),
            authorization,
            transport,
            ensured: Mutex::new(HashSet::new()),
            page_size: 1000,
            collections: RwLock::new(std::collections::HashMap::new()),
        })
    }

    /// Documents per `_find` request when a read has no limit or counts matches.
    pub fn with_page_size(mut self, page_size: usize) -> Self {
        self.page_size = page_size.max(1);
        self
    }

    fn call(&self, method: &str, path: &str, body: Option<&Json>) -> Result<Reply, String> {
        let mut headers = vec![("accept".to_string(), "application/json".to_string())];
        if body.is_some() {
            headers.push(("content-type".to_string(), "application/json".to_string()));
        }
        if let Some(authorization) = &self.authorization {
            headers.push(("authorization".to_string(), authorization.clone()));
        }
        let request = HttpRequest {
            method: method.to_string(),
            url: format!("{}{path}", self.base),
            headers,
            body: body.map(|b| b.to_string().into_bytes()).unwrap_or_default(),
            timeout: None,
        };
        let response = self
            .transport
            .send(&request)
            .map_err(|e| e.to_string())?;
        let body = serde_json::from_slice(&response.body).unwrap_or(Json::Null);
        Ok(Reply {
            status: response.status,
            body,
        })
    }

    fn fail<T>(reply: &Reply, what: &str) -> Result<T, String> {
        let reason = reply
            .body
            .get("reason")
            .and_then(Json::as_str)
            .unwrap_or("");
        Err(format!(
            "CouchDB {what} answered {}: {reason}",
            reply.status
        ))
    }

    fn db_name(&self, entity_type: &str) -> String {
        storage_name_for(&self.collections, entity_type)
    }

    fn db_path(&self, entity_type: &str) -> String {
        format!("/{}", encode_uri_component(&self.db_name(entity_type)))
    }

    fn doc_path(&self, entity_type: &str, id: &str) -> String {
        format!("{}/{}", self.db_path(entity_type), encode_uri_component(id))
    }

    fn ensure_database(&self, entity_type: &str) -> Result<(), String> {
        let name = self.db_name(entity_type);
        if self.ensured.lock().contains(&name) {
            return Ok(());
        }
        let reply = self.call("PUT", &self.db_path(entity_type), None)?;
        // 412 is CouchDB's answer for a database that exists; 409 is accepted as the TS adapter accepts it.
        if !matches!(reply.status, 201 | 202 | 409 | 412) {
            return Self::fail(&reply, "create database");
        }
        self.ensured.lock().insert(name);
        Ok(())
    }

    fn get_doc(&self, entity_type: &str, id: &str) -> Result<Option<Json>, String> {
        let reply = self.call("GET", &self.doc_path(entity_type, id), None)?;
        match reply.status {
            200 => Ok(Some(reply.body)),
            404 => Ok(None),
            _ => Self::fail(&reply, "get"),
        }
    }

    /// Writes `doc` (`PUT`): the new `_rev` on success, `None` on a `_rev` conflict.
    fn put_doc(
        &self,
        entity_type: &str,
        id: &str,
        doc: &Map<String, Json>,
    ) -> Result<Option<()>, String> {
        let reply = self.call(
            "PUT",
            &self.doc_path(entity_type, id),
            Some(&Json::Object(doc.clone())),
        )?;
        match reply.status {
            200..=299 => Ok(Some(())),
            _ if conflict(&reply) => Ok(None),
            _ => Self::fail(&reply, "write"),
        }
    }

    fn mango_find(&self, entity_type: &str, query: &Json) -> Result<Json, String> {
        let reply = self.call(
            "POST",
            &format!("{}/_find", self.db_path(entity_type)),
            Some(query),
        )?;
        match reply.status {
            200 => Ok(reply.body),
            404 => Ok(json!({ "docs": [] })),
            _ => Self::fail(&reply, "_find"),
        }
    }

    /// Every document the query matches, read `page_size` at a time through the bookmark (a query's `skip` applies to
    /// the first request only).
    fn find_all(&self, entity_type: &str, query: &Json) -> Result<Vec<Json>, String> {
        let mut out = Vec::new();
        let mut bookmark: Option<String> = None;
        loop {
            let mut request = query.clone();
            if let Some(map) = request.as_object_mut() {
                map.insert("limit".to_string(), json!(self.page_size));
                if let Some(bookmark) = &bookmark {
                    map.insert("bookmark".to_string(), json!(bookmark));
                    map.remove("skip");
                }
            }
            let page = self.mango_find(entity_type, &request)?;
            let docs = page
                .get("docs")
                .and_then(Json::as_array)
                .cloned()
                .unwrap_or_default();
            let count = docs.len();
            out.extend(docs);
            if count < self.page_size {
                return Ok(out);
            }
            bookmark = Some(
                page.get("bookmark")
                    .and_then(Json::as_str)
                    .ok_or("CouchDB _find returned a full page without a bookmark, so the next page cannot be requested")?
                    .to_string(),
            );
        }
    }

    /// The window's documents (`fields` limits what comes back) and the total: a second request only when the window
    /// cannot say it.
    fn window(
        &self,
        entity_type: &str,
        query: &RowQuery,
        fields: Option<Vec<&str>>,
    ) -> Option<Result<(Vec<Json>, usize), String>> {
        let (selector, sort) = mango_query_for(query)?;
        let offset = query.offset.unwrap_or(0);
        let mut shape = json!({ "selector": selector, "sort": sort });
        if let (Some(map), Some(fields)) = (shape.as_object_mut(), &fields) {
            map.insert("fields".to_string(), json!(fields));
        }
        Some((|| -> Result<(Vec<Json>, usize), String> {
            let docs = match query.limit {
                None => {
                    let mut all = shape.clone();
                    if offset > 0 {
                        all["skip"] = json!(offset);
                    }
                    self.find_all(entity_type, &all)?
                }
                Some(0) => Vec::new(),
                Some(limit) => {
                    let mut one = shape.clone();
                    one["skip"] = json!(offset);
                    one["limit"] = json!(limit);
                    self.mango_find(entity_type, &one)?
                        .get("docs")
                        .and_then(Json::as_array)
                        .cloned()
                        .unwrap_or_default()
                }
            };
            if query.total == Some(RowTotalMode::None) {
                let total = docs.len();
                return Ok((docs, total));
            }
            if let Some(total) = total_from_window(docs.len(), offset, query.limit) {
                return Ok((docs, total));
            }
            let counted = self
                .find_all(
                    entity_type,
                    &json!({ "selector": selector, "fields": ["_id"] }),
                )?
                .len();
            Ok((docs, counted))
        })())
    }

    fn write_create(&self, entity_type: &str, data: &Value) -> Result<RowWriteOutcome, String> {
        let Some(id) = id_of(data) else {
            return Ok(RowWriteOutcome::Failed {
                message: "create needs a string id".to_string(),
            });
        };
        self.ensure_database(entity_type)?;
        let body = doc_body(data, &id);
        match self.put_doc(entity_type, &id, &body)? {
            Some(()) => Ok(RowWriteOutcome::Created {
                row: doc_to_row(&Json::Object(body)),
            }),
            None => Ok(RowWriteOutcome::DuplicateId),
        }
    }

    /// Create-or-replace by id: the document is read with its `_rev` and the new body is written over it conditioned on
    /// that `_rev` (the existing `createdAt` kept); an absent document is inserted, and a write that loses the race
    /// re-reads.
    fn write_upsert(&self, entity_type: &str, data: &Value) -> Result<RowWriteOutcome, String> {
        let Some(id) = id_of(data) else {
            return Ok(RowWriteOutcome::Failed {
                message: "upsert needs a string id".to_string(),
            });
        };
        self.ensure_database(entity_type)?;
        for _ in 0..WRITE_ATTEMPTS {
            let current = self.get_doc(entity_type, &id)?;
            let mut body = doc_body(data, &id);
            if let Some(kept) = current.as_ref().and_then(|c| c.get("createdAt")) {
                body.insert("createdAt".to_string(), kept.clone());
            }
            if let Some(current) = &current {
                body.insert("_rev".to_string(), Json::String(rev_of(current)));
            }
            if self.put_doc(entity_type, &id, &body)?.is_some() {
                let row = doc_to_row(&Json::Object(body));
                return Ok(if current.is_some() {
                    RowWriteOutcome::Updated { row, before: None }
                } else {
                    RowWriteOutcome::Created { row }
                });
            }
        }
        Ok(RowWriteOutcome::Failed {
            message: format!("{entity_type} {id} changed concurrently {WRITE_ATTEMPTS} times"),
        })
    }

    /// Read the document with its `_rev`, test the guard on it, write conditioned on that `_rev`. A write that loses
    /// the race re-reads and re-tests, so a guard is never judged on a stale row; a guard that now fails is `refused`.
    fn write_existing(&self, op: &RowWriteOp) -> Result<RowWriteOutcome, String> {
        let (entity_type, id, guard) = match op {
            RowWriteOp::Update {
                entity_type,
                id,
                guard,
                ..
            }
            | RowWriteOp::Delete {
                entity_type,
                id,
                guard,
            } => (entity_type, id, guard),
            _ => return Err("write_existing takes an update or a delete".to_string()),
        };
        for _ in 0..WRITE_ATTEMPTS {
            let Some(current) = self.get_doc(entity_type, id)? else {
                return Ok(RowWriteOutcome::NotFound);
            };
            let before = doc_to_row(&current);
            if let Some(guard) = guard {
                if !guard.matches(&before) {
                    return Ok(RowWriteOutcome::Refused);
                }
            }
            let guarded = guard.as_ref().map(|_| before.clone());
            let rev = rev_of(&current);
            match op {
                RowWriteOp::Delete { .. } => {
                    let reply = self.call(
                        "DELETE",
                        &format!(
                            "{}?rev={}",
                            self.doc_path(entity_type, id),
                            encode_uri_component(&rev)
                        ),
                        None,
                    )?;
                    match reply.status {
                        200..=299 => return Ok(RowWriteOutcome::Deleted { before: guarded }),
                        _ if conflict(&reply) => {}
                        _ => return Self::fail(&reply, "delete"),
                    }
                }
                RowWriteOp::Update { data, .. } => {
                    let mut next = current.as_object().cloned().unwrap_or_default();
                    if let Json::Object(changes) = to_json(data) {
                        for (key, value) in changes {
                            if key != "id" {
                                next.insert(key, value);
                            }
                        }
                    }
                    next.insert("_id".to_string(), Json::String(id.clone()));
                    next.insert("_rev".to_string(), Json::String(rev));
                    if self.put_doc(entity_type, id, &next)?.is_some() {
                        return Ok(RowWriteOutcome::Updated {
                            row: doc_to_row(&Json::Object(next)),
                            before: guarded,
                        });
                    }
                }
                _ => {}
            }
        }
        Ok(RowWriteOutcome::Failed {
            message: format!("{entity_type} {id} changed concurrently {WRITE_ATTEMPTS} times"),
        })
    }

    fn write_one(&self, op: &RowWriteOp) -> RowWriteOutcome {
        let outcome = match op {
            RowWriteOp::Create { entity_type, data } => self.write_create(entity_type, data),
            RowWriteOp::Upsert { entity_type, data } => self.write_upsert(entity_type, data),
            RowWriteOp::Rekey { .. } => Err("rekey is unsupported by this store".to_string()),
            _ => self.write_existing(op),
        };
        outcome.unwrap_or_else(|message| RowWriteOutcome::Failed { message })
    }

    pub fn query_capabilities(&self) -> QueryCapabilities {
        query_capabilities()
    }

    pub fn write_capabilities(&self) -> WriteCapabilities {
        write_capabilities()
    }
}

impl EntitySource for CouchRows {
    fn bind_collection(&self, entity_type: &str, collection: &str) {
        self.collections
            .write()
            .insert(entity_type.to_string(), collection.to_string());
    }

    fn read(&self, entity_type: &str, id: &str) -> Result<Arc<Value>, String> {
        Ok(self
            .find(entity_type, id)?
            .unwrap_or_else(|| Arc::new(Value::Null)))
    }

    fn find(&self, entity_type: &str, id: &str) -> Result<Option<Arc<Value>>, String> {
        Ok(self
            .get_doc(entity_type, id)?
            .map(|doc| Arc::new(doc_to_row(&doc))))
    }

    /// A merge write; a missing row is created with its id.
    fn write(&self, entity_type: &str, id: &str, data: &Value) -> Result<Value, String> {
        for _ in 0..WRITE_ATTEMPTS {
            let Some(current) = self.get_doc(entity_type, id)? else {
                let mut with_id = data.as_object().cloned().unwrap_or_default();
                with_id.insert("id".to_string(), Value::String(id.to_string()));
                return self.create(entity_type, &Value::Object(with_id));
            };
            let mut next = current.as_object().cloned().unwrap_or_default();
            if let Json::Object(changes) = to_json(data) {
                for (key, value) in changes {
                    if key != "id" {
                        next.insert(key, value);
                    }
                }
            }
            next.insert("_rev".to_string(), Json::String(rev_of(&current)));
            if self.put_doc(entity_type, id, &next)?.is_some() {
                return Ok(doc_to_row(&Json::Object(next)));
            }
        }
        Err(format!(
            "{entity_type} {id} changed concurrently {WRITE_ATTEMPTS} times"
        ))
    }

    fn create(&self, entity_type: &str, data: &Value) -> Result<Value, String> {
        self.ensure_database(entity_type)?;
        let id = id_of(data).unwrap_or_else(mint_id);
        let body = doc_body(data, &id);
        match self.put_doc(entity_type, &id, &body)? {
            Some(()) => Ok(doc_to_row(&Json::Object(body))),
            None => Err(format!("Entity {entity_type} with id {id} already exists")),
        }
    }

    fn delete(&self, entity_type: &str, id: &str) -> Result<(), String> {
        let Some(current) = self.get_doc(entity_type, id)? else {
            return Ok(());
        };
        let reply = self.call(
            "DELETE",
            &format!(
                "{}?rev={}",
                self.doc_path(entity_type, id),
                encode_uri_component(&rev_of(&current))
            ),
            None,
        )?;
        match reply.status {
            200..=299 => Ok(()),
            _ => Self::fail(&reply, "delete"),
        }
    }

    /// A remote store is not cleared by the kernel.
    fn clear(&self) {}

    fn count(&self) -> usize {
        0
    }

    fn list(&self, entity_type: &str) -> Result<Vec<Arc<Value>>, String> {
        let reply = self.call(
            "GET",
            &format!("{}/_all_docs?include_docs=true", self.db_path(entity_type)),
            None,
        )?;
        match reply.status {
            200 => Ok(reply
                .body
                .get("rows")
                .and_then(Json::as_array)
                .into_iter()
                .flatten()
                .filter_map(|row| row.get("doc"))
                .map(|doc| Arc::new(doc_to_row(doc)))
                .collect()),
            404 => Ok(Vec::new()),
            _ => Self::fail(&reply, "list"),
        }
    }

    fn query_page(&self, entity_type: &str, query: &RowQuery) -> Option<Result<RowPage, String>> {
        self.window(entity_type, query, None).map(|run| {
            run.map(|(docs, total)| RowPage {
                rows: docs.iter().map(doc_to_row).collect(),
                total,
            })
        })
    }

    fn query_ids(&self, entity_type: &str, query: &RowQuery) -> Option<Result<RowIdPage, String>> {
        self.window(entity_type, query, Some(vec!["_id"]))
            .map(|run| {
                run.map(|(docs, total)| RowIdPage {
                    ids: docs
                        .iter()
                        .filter_map(|d| d.get("_id").and_then(Json::as_str).map(str::to_string))
                        .collect(),
                    total,
                })
            })
    }

    fn write_where(&self, op: &RowWriteOp) -> Option<Result<RowWriteOutcome, String>> {
        Some(Ok(self.write_one(op)))
    }

    /// Ordered writes, each its own `_rev`-guarded write; a failure stops the run with earlier writes kept. CouchDB
    /// cannot make a batch atomic, so an atomic batch is not answered.
    fn write_batch(&self, batch: &RowWriteBatch) -> Option<Result<Vec<RowWriteOutcome>, String>> {
        if batch.ops.iter().any(|op| matches!(op, RowWriteOp::Rekey { .. })) {
            return Some(Err("rekey is unsupported by this store".to_string()));
        }
        if batch.on_failure == RowWriteFailureMode::Atomic {
            return None;
        }
        let mut outcomes = Vec::new();
        for op in &batch.ops {
            let outcome = self.write_one(op);
            let applied = outcome.applied();
            outcomes.push(outcome);
            if !applied {
                break;
            }
        }
        Some(Ok(outcomes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_service::{HttpResponse, TransportError};
    use std::collections::VecDeque;

    struct Script {
        replies: Mutex<VecDeque<(u16, String)>>,
        sent: Mutex<Vec<HttpRequest>>,
    }

    impl Script {
        fn new(replies: &[(u16, Json)]) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(replies.iter().map(|(s, b)| (*s, b.to_string())).collect()),
                sent: Mutex::new(Vec::new()),
            })
        }

        fn sent(&self) -> Vec<(String, String, Json)> {
            self.sent
                .lock()
                .iter()
                .map(|r| {
                    (
                        r.method.clone(),
                        r.url.clone(),
                        serde_json::from_slice(&r.body).unwrap_or(Json::Null),
                    )
                })
                .collect()
        }
    }

    impl Transport for Script {
        fn send(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
            self.sent.lock().push(request.clone());
            let (status, body) = self
                .replies
                .lock()
                .pop_front()
                .ok_or_else(|| TransportError::Failed("script exhausted".into()))?;
            Ok(HttpResponse {
                status,
                headers: Vec::new(),
                body: body.into_bytes(),
            })
        }
    }

    fn q(json: Json) -> RowQuery {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn a_query_is_an_id_anchored_selector_sorted_on_the_document_id() {
        let (selector, sort) = mango_query_for(&q(json!({
            "filter": { "kind": "all", "of": [
                { "kind": "compare", "field": "priority", "op": ">=", "value": 3 },
                { "kind": "compare", "field": "status", "op": "!=", "value": "done" } ] },
            "order": { "field": "id", "direction": "desc" }
        })))
        .unwrap();
        assert_eq!(
            selector,
            json!({ "$and": [
                { "_id": { "$gt": null } },
                { "$and": [
                    { "priority": { "$gte": 3, "$type": "number" } },
                    { "$or": [{ "status": { "$exists": false } }, { "status": { "$ne": "done" } }] } ] } ] })
        );
        assert_eq!(sort, json!([{ "_id": "desc" }]));
    }

    #[test]
    fn what_mango_cannot_express_with_the_shared_semantics_is_declined() {
        let by_id = json!({ "field": "id", "direction": "asc" });
        for filter in [
            json!({ "kind": "contains", "field": "t", "text": "x", "caseSensitive": true }),
            json!({ "kind": "compare", "field": "t", "op": "<", "value": "m" }),
        ] {
            assert!(
                mango_query_for(&q(json!({ "filter": filter, "order": by_id }))).is_none(),
                "{filter}"
            );
        }
        assert!(
            mango_query_for(&q(
                json!({ "order": { "field": "title", "direction": "asc" } })
            ))
            .is_none(),
            "only id orders"
        );
        assert!(
            mango_query_for(&q(json!({}))).is_none(),
            "an unordered query means createdAt, which is not orderable here"
        );
        assert!(
            mango_query_for(&q(json!({ "order": by_id }))).is_some(),
            "control: an id order plans"
        );
    }

    #[test]
    fn a_not_over_an_ordering_leaf_includes_the_documents_that_lack_the_field() {
        let (selector, _) = mango_query_for(&q(json!({
            "filter": { "kind": "not", "of": { "kind": "compare", "field": "p", "op": ">", "value": 3 } },
            "order": { "field": "id", "direction": "asc" }
        })))
        .unwrap();
        let negated = &selector["$and"][1];
        assert_eq!(
            *negated,
            json!({ "$or": [
                { "p": { "$exists": false } },
                { "p": { "$not": { "$type": "number" } } },
                { "p": { "$lte": 3, "$type": "number" } } ] })
        );
    }

    #[test]
    fn a_database_is_named_like_the_postgres_table_and_credentials_become_a_basic_header() {
        let script = Script::new(&[(404, json!({ "error": "not_found" }))]);
        let rows =
            CouchRows::new("http://admin:p%40ss@couch.internal:5984/", script.clone()).unwrap();
        assert!(rows.find("TimeEntry", "a b").unwrap().is_none());
        let sent = script.sent.lock()[0].clone();
        assert_eq!(sent.url, "http://couch.internal:5984/time_entries/a%20b");
        let header = sent
            .headers
            .iter()
            .find(|(k, _)| k == "authorization")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert_eq!(header, format!("Basic {}", base64_encode(b"admin:p@ss")));
        assert!(
            CouchRows::new("ftp://x", Script::new(&[])).is_err(),
            "control: another scheme is refused"
        );
    }

    #[test]
    fn a_declared_collection_names_the_database_and_an_undeclared_entity_keeps_the_table_name() {
        let script = Script::new(&[
            (404, json!({ "error": "not_found" })),
            (404, json!({ "error": "not_found" })),
        ]);
        let rows = CouchRows::new("http://c:5984", script.clone()).unwrap();
        rows.bind_collection("TimeEntry", "timesheet");
        assert!(rows.find("TimeEntry", "a").unwrap().is_none());
        assert!(rows.find("Stock", "a").unwrap().is_none());
        let sent = script.sent();
        assert_eq!(sent[0].1, "http://c:5984/timesheet/a");
        assert_eq!(
            sent[1].1, "http://c:5984/stocks/a",
            "control: no declared collection keeps the pluralized name"
        );
    }

    #[test]
    fn a_create_makes_the_database_once_and_a_taken_id_is_a_duplicate() {
        let script = Script::new(&[
            (201, json!({ "ok": true })),
            (201, json!({ "ok": true, "id": "s1", "rev": "1-a" })),
            (409, json!({ "error": "conflict" })),
        ]);
        let rows = CouchRows::new("http://c:5984", script.clone()).unwrap();
        let op = |id: &str| RowWriteOp::Create {
            entity_type: "Stock".into(),
            data: from_json(&json!({ "id": id, "stock": 1 })),
        };
        assert!(matches!(
            rows.write_where(&op("s1")).unwrap().unwrap(),
            RowWriteOutcome::Created { .. }
        ));
        assert_eq!(
            rows.write_where(&op("s1")).unwrap().unwrap(),
            RowWriteOutcome::DuplicateId
        );
        let sent = script.sent();
        assert_eq!(
            (sent[0].0.as_str(), sent[0].1.as_str()),
            ("PUT", "http://c:5984/stocks")
        );
        assert_eq!(
            (
                sent[1].2["_id"].as_str(),
                sent[1].2["id"].as_str(),
                sent[1].2["stock"].as_f64()
            ),
            (Some("s1"), Some("s1"), Some(1.0))
        );
        assert_eq!(sent.len(), 3, "the database is created once");
    }

    #[test]
    fn a_guarded_update_reads_the_rev_tests_the_guard_and_retries_a_lost_race() {
        let doc =
            |rev: &str, stock: u32| json!({ "_id": "s1", "_rev": rev, "id": "s1", "stock": stock });
        let script = Script::new(&[
            (200, doc("1-a", 1)),
            (409, json!({ "error": "conflict" })),
            (200, doc("2-b", 0)),
        ]);
        let rows = CouchRows::new("http://c:5984", script.clone()).unwrap();
        let op: RowWriteOp = serde_json::from_value(json!({
            "kind": "update", "entityType": "Stock", "id": "s1", "data": { "stock": 0 },
            "guard": { "kind": "compare", "field": "stock", "op": ">", "value": 0 }
        }))
        .unwrap();
        assert_eq!(
            rows.write_where(&op).unwrap().unwrap(),
            RowWriteOutcome::Refused,
            "the lost race is re-read and the guard re-tested on the row that won"
        );
        let sent = script.sent();
        assert_eq!(sent[1].0, "PUT");
        assert_eq!(
            sent[1].2["_rev"], "1-a",
            "the write is conditioned on the rev it read"
        );
        assert_eq!(sent.len(), 3);
    }

    #[test]
    fn an_atomic_batch_is_not_answered_and_a_stop_run_keeps_earlier_writes() {
        let script = Script::new(&[(404, json!({ "error": "not_found" }))]);
        let rows = CouchRows::new("http://c:5984", script.clone()).unwrap();
        let atomic = RowWriteBatch {
            ops: vec![],
            on_failure: RowWriteFailureMode::Atomic,
        };
        assert!(rows.write_batch(&atomic).is_none());
        let stop = RowWriteBatch {
            ops: vec![RowWriteOp::Delete {
                entity_type: "Stock".into(),
                id: "nope".into(),
                guard: None,
            }],
            on_failure: RowWriteFailureMode::Stop,
        };
        assert_eq!(
            rows.write_batch(&stop).unwrap().unwrap(),
            vec![RowWriteOutcome::NotFound]
        );
    }
    #[test]
    fn rekey_is_refused_before_any_database_request_or_successful_prefix() {
        let script = Script::new(&[]);
        let rows = CouchRows::new("http://c:5984", script.clone()).unwrap();
        let rekey: RowWriteOp = serde_json::from_value(
            json!({"kind":"rekey","entityType":"Item","id":"a","newId":"b"}),
        )
        .unwrap();
        assert!(
            matches!(rows.write_where(&rekey), Some(Ok(RowWriteOutcome::Failed { ref message })) if message == "rekey is unsupported by this store")
        );
        for on_failure in [RowWriteFailureMode::Stop, RowWriteFailureMode::Atomic] {
            let create = RowWriteOp::Create {
                entity_type: "Item".into(),
                data: serde_json::from_value(json!({"id":"new"})).unwrap(),
            };
            let result = rows.write_batch(&RowWriteBatch {
                ops: vec![create, rekey.clone()],
                on_failure,
            });
            assert!(
                matches!(result, Some(Err(ref message)) if message == "rekey is unsupported by this store")
            );
        }
        assert!(script.sent().is_empty());
        assert_eq!(rows.write_capabilities().rekey, None);
    }
    fn sel_sort() -> (Json, Json) {
        mango_query_for(&q(json!({ "order": { "field": "id", "direction": "asc" } }))).unwrap()
    }

    fn rows_json(rows: &[Value]) -> Json {
        Json::Array(rows.iter().map(to_json).collect())
    }

    fn created_row(outcome: RowWriteOutcome) -> Json {
        match outcome {
            RowWriteOutcome::Created { row } | RowWriteOutcome::Updated { row, .. } => to_json(&row),
            other => panic!("expected a written row, got {other:?}"),
        }
    }

    fn failed(outcome: RowWriteOutcome) -> String {
        match outcome {
            RowWriteOutcome::Failed { message } => message,
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn std_twin_wire_sequence_matches_the_std_couchdb_atom() {
        let (sel, sort) = sel_sort();
        let current = |rev: &str, stock: u32| {
            json!({ "_id": "s1", "_rev": rev, "id": "s1", "stock": stock, "createdAt": "c0" })
        };
        let script = Script::new(&[
            (201, json!({ "ok": true })),
            (201, json!({ "ok": true })),
            (200, current("1-x", 1)),
            (200, current("1-x", 1)),
            (201, json!({ "ok": true })),
            (200, current("2-x", 0)),
            (201, json!({ "ok": true })),
            (200, json!({ "rows": [{ "id": "s1", "doc": current("3-x", 5) }] })),
            (200, json!({ "docs": [current("3-x", 5)], "bookmark": "bm" })),
            (200, json!({ "docs": [{ "_id": "s1" }], "bookmark": "bm" })),
            (200, current("3-x", 5)),
            (200, json!({ "ok": true })),
        ]);
        let rows = CouchRows::new("http://c:5984", script.clone()).unwrap();
        let created = rows
            .write_where(&RowWriteOp::Create {
                entity_type: "Stock".into(),
                data: from_json(&json!({ "id": "s1", "stock": 1, "createdAt": "c0" })),
            })
            .unwrap()
            .unwrap();
        assert_eq!(
            created_row(created),
            json!({ "id": "s1", "stock": 1, "createdAt": "c0" })
        );
        assert_eq!(
            to_json(&rows.find("Stock", "s1").unwrap().unwrap()),
            json!({ "id": "s1", "stock": 1, "createdAt": "c0" })
        );
        let update: RowWriteOp = serde_json::from_value(json!({
            "kind": "update", "entityType": "Stock", "id": "s1",
            "data": { "id": "zz", "stock": 0 }, "guard": null
        }))
        .unwrap();
        assert_eq!(
            created_row(rows.write_where(&update).unwrap().unwrap()),
            json!({ "id": "s1", "stock": 0, "createdAt": "c0" })
        );
        let upsert = RowWriteOp::Upsert {
            entity_type: "Stock".into(),
            data: from_json(&json!({ "id": "s1", "stock": 5 })),
        };
        assert_eq!(
            created_row(rows.write_where(&upsert).unwrap().unwrap()),
            json!({ "id": "s1", "stock": 5, "createdAt": "c0" })
        );
        assert_eq!(
            rows_json(
                &rows
                    .list("Stock")
                    .unwrap()
                    .iter()
                    .map(|r| (**r).clone())
                    .collect::<Vec<_>>()
            ),
            json!([{ "id": "s1", "stock": 5, "createdAt": "c0" }])
        );
        let page = rows
            .query_page(
                "Stock",
                &q(json!({ "order": { "field": "id", "direction": "asc" }, "limit": 1 })),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            (rows_json(&page.rows), page.total),
            (json!([{ "id": "s1", "stock": 5, "createdAt": "c0" }]), 1)
        );
        let deleted = RowWriteOp::Delete {
            entity_type: "Stock".into(),
            id: "s1".into(),
            guard: None,
        };
        assert_eq!(
            rows.write_where(&deleted).unwrap().unwrap(),
            RowWriteOutcome::Deleted { before: None }
        );
        let db = "http://c:5984/stocks";
        let doc = format!("{db}/s1");
        let expected: Vec<(&str, String, Json)> = vec![
            ("PUT", db.to_string(), Json::Null),
            ("PUT", doc.clone(), json!({ "stock": 1, "createdAt": "c0", "_id": "s1", "id": "s1" })),
            ("GET", doc.clone(), Json::Null),
            ("GET", doc.clone(), Json::Null),
            ("PUT", doc.clone(), json!({ "stock": 0, "createdAt": "c0", "_id": "s1", "id": "s1", "_rev": "1-x" })),
            ("GET", doc.clone(), Json::Null),
            ("PUT", doc.clone(), json!({ "id": "s1", "stock": 5, "createdAt": "c0", "_rev": "2-x", "_id": "s1" })),
            ("GET", format!("{db}/_all_docs?include_docs=true"), Json::Null),
            ("POST", format!("{db}/_find"), json!({ "selector": sel, "sort": sort, "skip": 0, "limit": 1 })),
            ("POST", format!("{db}/_find"), json!({ "selector": sel, "fields": ["_id"], "limit": 1000 })),
            ("GET", doc.clone(), Json::Null),
            ("DELETE", format!("{doc}?rev=3-x"), Json::Null),
        ];
        let sent = script.sent();
        assert_eq!(sent.len(), expected.len());
        for (got, want) in sent.iter().zip(&expected) {
            assert_eq!((got.0.as_str(), &got.1, &got.2), (want.0, &want.1, &want.2));
        }
        let requests = script.sent.lock();
        let content_type = |i: usize| {
            requests[i]
                .headers
                .iter()
                .any(|(k, v)| k == "content-type" && v == "application/json")
        };
        assert!(!content_type(0) && content_type(1) && !content_type(2));
        assert!(requests.iter().all(|r| r
            .headers
            .iter()
            .any(|(k, v)| k == "accept" && v == "application/json")));
    }

    #[test]
    fn std_twin_error_texts_match_the_std_couchdb_atom() {
        let unauth = json!({ "error": "unauthorized", "reason": "Name or password is incorrect." });
        let nope = json!({ "error": "x", "reason": "nope" });
        let doc = json!({ "_id": "s1", "_rev": "1-a", "id": "s1", "stock": 1 });
        let rows = |replies: &[(u16, Json)]| {
            CouchRows::new("http://c:5984", Script::new(replies)).unwrap()
        };
        let create = |id: Json| RowWriteOp::Create {
            entity_type: "Stock".into(),
            data: from_json(&json!({ "id": id, "stock": 1 })),
        };
        assert_eq!(
            rows(&[(401, unauth.clone())]).find("Stock", "s1").unwrap_err(),
            "CouchDB get answered 401: Name or password is incorrect."
        );
        assert_eq!(
            rows(&[(500, json!("upstream down"))]).find("Stock", "s1").unwrap_err(),
            "CouchDB get answered 500: "
        );
        assert_eq!(
            rows(&[(500, nope.clone())]).list("Stock").unwrap_err(),
            "CouchDB list answered 500: nope"
        );
        assert!(rows(&[(404, json!({ "error": "not_found" }))]).list("Stock").unwrap().is_empty());
        assert_eq!(
            failed(rows(&[(401, unauth.clone())]).write_where(&create(json!("s1"))).unwrap().unwrap()),
            "CouchDB create database answered 401: Name or password is incorrect."
        );
        assert_eq!(
            failed(rows(&[(200, json!({ "ok": true }))]).write_where(&create(json!("s1"))).unwrap().unwrap()),
            "CouchDB create database answered 200: "
        );
        for made in [201, 202, 409, 412] {
            let outcome = rows(&[(made, json!({ "ok": true })), (201, json!({ "ok": true }))])
                .write_where(&create(json!("s1")))
                .unwrap()
                .unwrap();
            assert!(matches!(outcome, RowWriteOutcome::Created { .. }), "database answer {made}");
        }
        assert_eq!(
            failed(
                rows(&[(201, json!({ "ok": true })), (500, json!({ "error": "x", "reason": "disk full" }))])
                    .write_where(&create(json!("s1")))
                    .unwrap()
                    .unwrap()
            ),
            "CouchDB write answered 500: disk full"
        );
        let script = Script::new(&[]);
        let idless = CouchRows::new("http://c:5984", script.clone()).unwrap();
        assert_eq!(
            failed(idless.write_where(&create(json!(""))).unwrap().unwrap()),
            "create needs a string id"
        );
        let upsert = RowWriteOp::Upsert {
            entity_type: "Stock".into(),
            data: from_json(&json!({ "stock": 1 })),
        };
        assert_eq!(
            failed(idless.write_where(&upsert).unwrap().unwrap()),
            "upsert needs a string id"
        );
        assert!(script.sent().is_empty(), "an id-less write sends nothing");
        let delete = RowWriteOp::Delete {
            entity_type: "Stock".into(),
            id: "s1".into(),
            guard: None,
        };
        assert_eq!(
            failed(rows(&[(200, doc.clone()), (500, nope.clone())]).write_where(&delete).unwrap().unwrap()),
            "CouchDB delete answered 500: nope"
        );
        assert_eq!(
            failed(rows(&[(500, nope.clone())]).write_where(&delete).unwrap().unwrap()),
            "CouchDB get answered 500: nope"
        );
        assert_eq!(
            rows(&[(404, nope.clone())]).write_where(&delete).unwrap().unwrap(),
            RowWriteOutcome::NotFound
        );
        let update: RowWriteOp = serde_json::from_value(json!({
            "kind": "update", "entityType": "Stock", "id": "s1", "data": { "stock": 0 }, "guard": null
        }))
        .unwrap();
        assert_eq!(
            rows(&[(404, nope.clone())]).write_where(&update).unwrap().unwrap(),
            RowWriteOutcome::NotFound
        );
        let racing: Vec<(u16, Json)> = (0..5)
            .flat_map(|_| [(200, doc.clone()), (409, json!({ "error": "conflict" }))])
            .collect();
        assert_eq!(
            failed(rows(&racing).write_where(&update).unwrap().unwrap()),
            "Stock s1 changed concurrently 5 times"
        );
        let racing_delete: Vec<(u16, Json)> = (0..5)
            .flat_map(|_| [(200, doc.clone()), (409, json!({ "error": "conflict" }))])
            .collect();
        assert_eq!(
            failed(rows(&racing_delete).write_where(&delete).unwrap().unwrap()),
            "Stock s1 changed concurrently 5 times"
        );
        let four_then_won: Vec<(u16, Json)> = (0..4)
            .flat_map(|_| [(200, doc.clone()), (409, json!({ "error": "conflict" }))])
            .chain([(200, doc.clone()), (201, json!({ "ok": true }))])
            .collect();
        assert!(matches!(
            rows(&four_then_won).write_where(&update).unwrap().unwrap(),
            RowWriteOutcome::Updated { .. }
        ));
    }

    #[test]
    fn std_twin_upsert_keeps_the_stored_created_at_and_reports_created_or_replaced() {
        let upsert = |data: Json| RowWriteOp::Upsert {
            entity_type: "Stock".into(),
            data: from_json(&data),
        };
        let found = json!({ "_id": "s1", "_rev": "3-c", "id": "s1", "stock": 9, "createdAt": "2026-01-01T00:00:00.000Z", "old": true });
        let replaced = CouchRows::new(
            "http://c:5984",
            Script::new(&[(201, json!({ "ok": true })), (200, found), (201, json!({ "ok": true }))]),
        )
        .unwrap()
        .write_where(&upsert(json!({ "id": "s1", "stock": 1, "createdAt": "2099-01-01T00:00:00.000Z" })))
        .unwrap()
        .unwrap();
        assert!(matches!(replaced, RowWriteOutcome::Updated { .. }));
        assert_eq!(
            created_row(replaced),
            json!({ "id": "s1", "stock": 1, "createdAt": "2026-01-01T00:00:00.000Z" })
        );
        let inserted = CouchRows::new(
            "http://c:5984",
            Script::new(&[
                (201, json!({ "ok": true })),
                (404, json!({ "error": "not_found" })),
                (201, json!({ "ok": true })),
            ]),
        )
        .unwrap()
        .write_where(&upsert(json!({ "id": "s1", "stock": 1 })))
        .unwrap()
        .unwrap();
        assert!(matches!(inserted, RowWriteOutcome::Created { .. }));
        assert_eq!(created_row(inserted), json!({ "id": "s1", "stock": 1 }));
    }

    #[test]
    fn std_twin_list_skips_a_row_without_a_doc() {
        let rows = CouchRows::new(
            "http://c:5984",
            Script::new(&[(
                200,
                json!({ "rows": [
                    { "id": "a", "doc": { "_id": "a", "_rev": "1-x", "id": "a" } },
                    { "id": "b", "key": "b" } ] }),
            )]),
        )
        .unwrap();
        let listed: Vec<Value> = rows.list("Stock").unwrap().iter().map(|r| (**r).clone()).collect();
        assert_eq!(rows_json(&listed), json!([{ "id": "a" }]));
    }

    #[test]
    fn std_twin_find_windows_totals_and_pages_match_the_std_couchdb_atom() {
        let d = |n: usize| json!({ "_id": format!("d{n}"), "_rev": "1-x", "id": format!("d{n}") });
        let r = |n: usize| json!({ "id": format!("d{n}") });
        let ids = |n: usize| -> Vec<Json> { (0..n).map(|i| json!({ "_id": format!("d{i}") })).collect() };
        let ordered = |extra: Json| {
            let mut query = json!({ "order": { "field": "id", "direction": "asc" } });
            for (k, v) in extra.as_object().unwrap() {
                query[k] = v.clone();
            }
            q(query)
        };
        let page = |rows: &CouchRows, query: &RowQuery| {
            let page = rows.query_page("Stock", query).unwrap().unwrap();
            (rows_json(&page.rows), page.total)
        };
        let make = |replies: &[(u16, Json)]| {
            let script = Script::new(replies);
            (CouchRows::new("http://c:5984", script.clone()).unwrap().with_page_size(2), script)
        };

        let (rows, script) = make(&[(200, json!({ "docs": [d(1), d(2)], "bookmark": "b1" })), (200, json!({ "docs": [d(3)], "bookmark": "b2" }))]);
        assert_eq!(page(&rows, &ordered(json!({ "offset": 2 }))), (json!([r(1), r(2), r(3)]), 5));
        let sent = script.sent();
        assert_eq!(sent[0].2["skip"], 2);
        assert_eq!(sent[1].2["bookmark"], "b1");
        assert!(sent[1].2.get("skip").is_none(), "a bookmark page carries no skip");

        let (rows, _) = make(&[(200, json!({ "docs": [d(1)], "bookmark": "n" }))]);
        assert_eq!(page(&rows, &ordered(json!({ "limit": 3, "offset": 2 }))), (json!([r(1)]), 3));

        let (rows, script) = make(&[(200, json!({ "docs": [], "bookmark": "n" }))]);
        assert_eq!(page(&rows, &ordered(json!({ "limit": 3 }))), (json!([]), 0));
        assert_eq!(script.sent().len(), 1);

        let (rows, script) = make(&[(200, json!({ "docs": [] })), (200, json!({ "docs": [] }))]);
        assert_eq!(page(&rows, &ordered(json!({ "limit": 3, "offset": 4 }))), (json!([]), 0));
        assert_eq!(script.sent().len(), 2, "an empty window past the offset is counted");

        let (rows, script) = make(&[(200, json!({ "docs": ids(1) }))]);
        assert_eq!(page(&rows, &ordered(json!({ "limit": 0 }))), (json!([]), 1));
        assert_eq!(script.sent().len(), 1, "limit 0 sends no window request");
        assert_eq!(script.sent()[0].2["fields"], json!(["_id"]));

        let (rows, _) = make(&[(200, json!({ "docs": [d(1), d(2)], "bookmark": "b" })), (200, json!({ "docs": ids(2), "bookmark": "c1" })), (200, json!({ "docs": ids(1), "bookmark": "c2" }))]);
        assert_eq!(page(&rows, &ordered(json!({ "limit": 2 }))), (json!([r(1), r(2)]), 3));

        let (rows, _) = make(&[(200, json!({ "docs": [d(1), d(2)], "bookmark": "b" })), (404, json!({ "error": "not_found" }))]);
        assert_eq!(page(&rows, &ordered(json!({}))), (json!([r(1), r(2)]), 2));

        let (rows, _) = make(&[(200, json!({ "docs": [d(1), d(2)], "bookmark": "b" })), (200, json!({ "docs": ids(2), "bookmark": "c1" })), (404, json!({ "error": "not_found" }))]);
        assert_eq!(page(&rows, &ordered(json!({ "limit": 2 }))), (json!([r(1), r(2)]), 2));

        let (rows, _) = make(&[(404, json!({ "error": "not_found" })), (404, json!({ "error": "not_found" }))]);
        assert_eq!(page(&rows, &ordered(json!({ "limit": 3, "offset": 5 }))), (json!([]), 0));

        let (rows, _) = make(&[(400, json!({ "error": "bad_request", "reason": "invalid selector" }))]);
        assert_eq!(
            rows.query_page("Stock", &ordered(json!({}))).unwrap().unwrap_err(),
            "CouchDB _find answered 400: invalid selector"
        );

        let (rows, _) = make(&[(200, json!({ "docs": [d(1), d(2)] }))]);
        assert_eq!(
            rows.query_page("Stock", &ordered(json!({}))).unwrap().unwrap_err(),
            "CouchDB _find returned a full page without a bookmark, so the next page cannot be requested"
        );

        let (rows, _) = make(&[(200, json!({ "docs": [d(1), d(2)], "bookmark": "b" })), (200, json!({ "docs": ids(2) }))]);
        assert_eq!(
            rows.query_page("Stock", &ordered(json!({ "limit": 2 }))).unwrap().unwrap_err(),
            "CouchDB _find returned a full page without a bookmark, so the next page cannot be requested"
        );
    }

    const STD_TWIN_SUPPORTED: &[(&str, &str, &str, &str)] = &[
        ("filter-and-desc-order-is-an-id-anchored-selector", r#"{"filter":{"kind":"all","of":[{"kind":"compare","field":"priority","op":">=","value":3},{"kind":"compare","field":"status","op":"!=","value":"done"}]},"order":{"field":"id","direction":"desc"}}"#, r#"{"$and":[{"_id":{"$gt":null}},{"$and":[{"priority":{"$gte":3,"$type":"number"}},{"$or":[{"status":{"$exists":false}},{"status":{"$ne":"done"}}]}]}]}"#, r#"[{"_id":"desc"}]"#),
        ("not-over-an-ordering-leaf-includes-documents-lacking-the-field", r#"{"filter":{"kind":"not","of":{"kind":"compare","field":"p","op":">","value":3}},"order":{"field":"id","direction":"asc"}}"#, r#"{"$and":[{"_id":{"$gt":null}},{"$or":[{"p":{"$exists":false}},{"p":{"$not":{"$type":"number"}}},{"p":{"$lte":3,"$type":"number"}}]}]}"#, r#"[{"_id":"asc"}]"#),
        ("policy-then-filter-are-two-parts", r#"{"policy":{"kind":"compare","field":"owner","op":"==","value":"u1"},"filter":{"kind":"compare","field":"n","op":"==","value":5},"order":{"field":"id","direction":"asc"}}"#, r#"{"$and":[{"_id":{"$gt":null}},{"owner":{"$eq":"u1"}},{"n":{"$eq":5}}]}"#, r#"[{"_id":"asc"}]"#),
        ("no-predicate-is-the-id-anchor-alone", r#"{"order":{"field":"id","direction":"asc"}}"#, r#"{"$and":[{"_id":{"$gt":null}}]}"#, r#"[{"_id":"asc"}]"#),
        ("equality-of-null-or-a-non-scalar-matches-nothing", r#"{"filter":{"kind":"all","of":[{"kind":"compare","field":"a","op":"==","value":null},{"kind":"compare","field":"b","op":"==","value":[1]}]},"order":{"field":"id","direction":"asc"}}"#, r#"{"$and":[{"_id":{"$gt":null}},{"$and":[{"_id":{"$exists":false}},{"_id":{"$exists":false}}]}]}"#, r#"[{"_id":"asc"}]"#),
        ("inequality-of-null-needs-the-field-present-and-non-null", r#"{"filter":{"kind":"compare","field":"a","op":"!=","value":null},"order":{"field":"id","direction":"asc"}}"#, r#"{"$and":[{"_id":{"$gt":null}},{"$and":[{"a":{"$exists":true}},{"a":{"$ne":null}}]}]}"#, r#"[{"_id":"asc"}]"#),
        ("inequality-of-a-non-scalar-matches-everything", r#"{"filter":{"kind":"compare","field":"a","op":"!=","value":{"k":1}},"order":{"field":"id","direction":"asc"}}"#, r#"{"$and":[{"_id":{"$gt":null}},{"_id":{"$exists":true}}]}"#, r#"[{"_id":"asc"}]"#),
        ("ordering-compare-of-a-non-scalar-matches-nothing", r#"{"filter":{"kind":"compare","field":"a","op":"<","value":[1]},"order":{"field":"id","direction":"asc"}}"#, r#"{"$and":[{"_id":{"$gt":null}},{"_id":{"$exists":false}}]}"#, r#"[{"_id":"asc"}]"#),
        ("ordering-on-a-boolean-pins-the-type", r#"{"filter":{"kind":"compare","field":"a","op":"<=","value":true},"order":{"field":"id","direction":"asc"}}"#, r#"{"$and":[{"_id":{"$gt":null}},{"a":{"$lte":true,"$type":"boolean"}}]}"#, r#"[{"_id":"asc"}]"#),
        ("not-over-equality-is-the-complement-and-not-over-null-ordering-matches-all", r#"{"filter":{"kind":"all","of":[{"kind":"not","of":{"kind":"compare","field":"a","op":"==","value":"x"}},{"kind":"not","of":{"kind":"compare","field":"b","op":"<","value":null}}]},"order":{"field":"id","direction":"asc"}}"#, r#"{"$and":[{"_id":{"$gt":null}},{"$and":[{"$or":[{"a":{"$exists":false}},{"a":{"$ne":"x"}}]},{"_id":{"$exists":true}}]}]}"#, r#"[{"_id":"asc"}]"#),
        ("member-keeps-only-scalars-and-empty-matches-nothing", r#"{"filter":{"kind":"all","of":[{"kind":"member","field":"s","values":["a",1,true,null,[2]]},{"kind":"member","field":"t","values":[null]}]},"order":{"field":"id","direction":"asc"}}"#, r#"{"$and":[{"_id":{"$gt":null}},{"$and":[{"s":{"$in":["a",1,true]}},{"_id":{"$exists":false}}]}]}"#, r#"[{"_id":"asc"}]"#),
        ("not-over-member-leads-with-missing", r#"{"filter":{"kind":"all","of":[{"kind":"not","of":{"kind":"member","field":"s","values":["a",2]}},{"kind":"not","of":{"kind":"member","field":"t","values":[]}}]},"order":{"field":"id","direction":"asc"}}"#, r#"{"$and":[{"_id":{"$gt":null}},{"$and":[{"$or":[{"s":{"$exists":false}},{"s":{"$nin":["a",2]}}]},{"_id":{"$exists":true}}]}]}"#, r#"[{"_id":"asc"}]"#),
        ("has-element-and-its-negation", r#"{"filter":{"kind":"all","of":[{"kind":"hasElement","field":"tags","value":"x"},{"kind":"not","of":{"kind":"hasElement","field":"tags","value":"y"}},{"kind":"hasElement","field":"tags","value":null},{"kind":"not","of":{"kind":"hasElement","field":"tags","value":{"k":1}}}]},"order":{"field":"id","direction":"asc"}}"#, r#"{"$and":[{"_id":{"$gt":null}},{"$and":[{"tags":{"$elemMatch":{"$eq":"x"}}},{"$or":[{"tags":{"$exists":false}},{"tags":{"$not":{"$elemMatch":{"$eq":"y"}}}}]},{"_id":{"$exists":false}},{"_id":{"$exists":true}}]}]}"#, r#"[{"_id":"asc"}]"#),
        ("is-null-and-its-negation", r#"{"filter":{"kind":"all","of":[{"kind":"isNull","field":"a","isNull":true},{"kind":"isNull","field":"b","isNull":false},{"kind":"not","of":{"kind":"isNull","field":"c","isNull":true}}]},"order":{"field":"id","direction":"asc"}}"#, r#"{"$and":[{"_id":{"$gt":null}},{"$and":[{"$or":[{"a":{"$exists":false}},{"a":{"$eq":null}}]},{"$and":[{"b":{"$exists":true}},{"b":{"$ne":null}}]},{"$and":[{"c":{"$exists":true}},{"c":{"$ne":null}}]}]}]}"#, r#"[{"_id":"asc"}]"#),
        ("constants-and-empty-combinators", r#"{"filter":{"kind":"all","of":[{"kind":"const","value":true},{"kind":"const","value":false},{"kind":"all","of":[]},{"kind":"any","of":[]},{"kind":"not","of":{"kind":"const","value":true}},{"kind":"not","of":{"kind":"all","of":[]}},{"kind":"not","of":{"kind":"any","of":[]}}]},"order":{"field":"id","direction":"asc"}}"#, r#"{"$and":[{"_id":{"$gt":null}},{"$and":[{"_id":{"$exists":true}},{"_id":{"$exists":false}},{"_id":{"$exists":true}},{"_id":{"$exists":false}},{"_id":{"$exists":false}},{"_id":{"$exists":false}},{"_id":{"$exists":true}}]}]}"#, r#"[{"_id":"asc"}]"#),
        ("de-morgan-pushes-not-through-all-and-any", r#"{"filter":{"kind":"all","of":[{"kind":"not","of":{"kind":"all","of":[{"kind":"compare","field":"a","op":"==","value":1},{"kind":"compare","field":"b","op":"==","value":2}]}},{"kind":"not","of":{"kind":"any","of":[{"kind":"compare","field":"a","op":"==","value":1},{"kind":"compare","field":"b","op":"==","value":2}]}},{"kind":"not","of":{"kind":"not","of":{"kind":"compare","field":"c","op":"==","value":3}}}]},"order":{"field":"id","direction":"asc"}}"#, r#"{"$and":[{"_id":{"$gt":null}},{"$and":[{"$or":[{"$or":[{"a":{"$exists":false}},{"a":{"$ne":1}}]},{"$or":[{"b":{"$exists":false}},{"b":{"$ne":2}}]}]},{"$and":[{"$or":[{"a":{"$exists":false}},{"a":{"$ne":1}}]},{"$or":[{"b":{"$exists":false}},{"b":{"$ne":2}}]}]},{"c":{"$eq":3}}]}]}"#, r#"[{"_id":"asc"}]"#),
        ("an-ids-query-and-a-window-and-no-total", r#"{"filter":{"kind":"compare","field":"n","op":"==","value":1},"order":{"field":"id","direction":"asc"},"offset":2,"limit":5,"total":"none"}"#, r#"{"$and":[{"_id":{"$gt":null}},{"n":{"$eq":1}}]}"#, r#"[{"_id":"asc"}]"#),
        ("a-string-equality-is-supported", r#"{"filter":{"kind":"compare","field":"t","op":"==","value":"m"},"order":{"field":"id","direction":"asc"}}"#, r#"{"$and":[{"_id":{"$gt":null}},{"t":{"$eq":"m"}}]}"#, r#"[{"_id":"asc"}]"#),
    ];

    const STD_TWIN_DECLINED: &[(&str, &str, &str)] = &[
        ("declined-contains-leaf", r#"{"filter":{"kind":"contains","field":"t","text":"x","caseSensitive":true},"order":{"field":"id","direction":"asc"}}"#, "leaf-unsupported"),
        ("declined-contains-leaf-under-not-and-any", r#"{"filter":{"kind":"any","of":[{"kind":"const","value":true},{"kind":"not","of":{"kind":"contains","field":"t","text":"x","caseSensitive":true}}]},"order":{"field":"id","direction":"asc"}}"#, "leaf-unsupported"),
        ("declined-contains-leaf-in-the-policy", r#"{"policy":{"kind":"contains","field":"t","text":"x","caseSensitive":true},"order":{"field":"id","direction":"asc"}}"#, "leaf-unsupported"),
        ("declined-string-lt", r#"{"filter":{"kind":"compare","field":"t","op":"<","value":"m"},"order":{"field":"id","direction":"asc"}}"#, "value-kind-unsupported"),
        ("declined-string-le", r#"{"filter":{"kind":"compare","field":"t","op":"<=","value":"m"},"order":{"field":"id","direction":"asc"}}"#, "value-kind-unsupported"),
        ("declined-string-gt", r#"{"filter":{"kind":"compare","field":"t","op":">","value":"m"},"order":{"field":"id","direction":"asc"}}"#, "value-kind-unsupported"),
        ("declined-string-ge", r#"{"filter":{"kind":"compare","field":"t","op":">=","value":"m"},"order":{"field":"id","direction":"asc"}}"#, "value-kind-unsupported"),
        ("declined-string-ordering-under-not", r#"{"filter":{"kind":"not","of":{"kind":"compare","field":"t","op":"<","value":"m"}},"order":{"field":"id","direction":"asc"}}"#, "value-kind-unsupported"),
        ("declined-leaf-before-value-kind", r#"{"filter":{"kind":"all","of":[{"kind":"compare","field":"t","op":"<","value":"m"},{"kind":"contains","field":"t","text":"x","caseSensitive":true}]},"order":{"field":"id","direction":"asc"}}"#, "leaf-unsupported"),
        ("declined-value-kind-before-nearest", r#"{"filter":{"kind":"compare","field":"t","op":"<","value":"m"},"order":{"field":"id","direction":"asc"},"nearest":{"field":"v","vector":[1],"k":2}}"#, "value-kind-unsupported"),
        ("declined-nearest", r#"{"order":{"field":"id","direction":"asc"},"nearest":{"field":"v","vector":[1],"k":2}}"#, "nearest"),
        ("declined-nearest-before-order-field", r#"{"order":{"field":"title","direction":"asc"},"nearest":{"field":"v","vector":[1],"k":2}}"#, "nearest"),
        ("declined-order-by-another-field", r#"{"order":{"field":"title","direction":"asc"}}"#, "order-field"),
        ("declined-no-order-means-created-at", r#"{}"#, "order-field"),
    ];

    #[test]
    fn std_twin_mango_selectors_and_declines_match_the_std_couchdb_atom() {
        for (id, query, selector, sort) in STD_TWIN_SUPPORTED {
            let query = q(serde_json::from_str(query).unwrap());
            assert_eq!(
                query_unsupported_reason(&query_capabilities(), &query).map(|r| r.as_str()),
                None,
                "{id}"
            );
            let (got_selector, got_sort) = mango_query_for(&query).unwrap();
            assert_eq!(
                got_selector,
                serde_json::from_str::<Json>(selector).unwrap(),
                "{id}"
            );
            assert_eq!(got_sort, serde_json::from_str::<Json>(sort).unwrap(), "{id}");
        }
        for (id, query, reason) in STD_TWIN_DECLINED {
            let query = q(serde_json::from_str(query).unwrap());
            assert_eq!(
                query_unsupported_reason(&query_capabilities(), &query).map(|r| r.as_str()),
                Some(*reason),
                "{id}"
            );
            assert!(mango_query_for(&query).is_none(), "{id}");
        }
    }

    fn std_twin_rows(replies: &[(u16, Json)]) -> (CouchRows, Arc<Script>) {
        let script = Script::new(replies);
        (
            CouchRows::new("http://c:5984", script.clone()).unwrap(),
            script,
        )
    }

    fn std_twin_op(json: Json) -> RowWriteOp {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn std_twin_a_database_is_named_from_the_entity_type_or_its_last_declared_collection() {
        for (entity_type, database) in [
            ("TimeEntry", "time_entries"),
            ("Stock", "stocks"),
            ("Class", "classes"),
            ("Category", "categories"),
            ("Person", "persons"),
            ("Item2Name", "item2_names"),
            ("XMLHttp", "xmlhttps"),
            ("Day", "daies"),
            ("Bus", "buses"),
        ] {
            let (rows, script) = std_twin_rows(&[(404, json!({ "error": "not_found" }))]);
            assert!(rows.find(entity_type, "a b").unwrap().is_none());
            assert_eq!(
                script.sent()[0].1,
                format!("http://c:5984/{database}/a%20b"),
                "{entity_type}"
            );
        }
        let (rows, script) = std_twin_rows(&[
            (404, json!({ "error": "not_found" })),
            (404, json!({ "error": "not_found" })),
        ]);
        rows.bind_collection("TimeEntry", "first");
        rows.bind_collection("TimeEntry", "second");
        assert!(rows.find("TimeEntry", "a").unwrap().is_none());
        assert!(rows.find("Stock", "a").unwrap().is_none());
        let sent = script.sent();
        assert_eq!(sent[0].1, "http://c:5984/second/a");
        assert_eq!(sent[1].1, "http://c:5984/stocks/a");
    }

    #[test]
    fn std_twin_find_request_bodies_totals_and_ids_match_the_std_couchdb_atom() {
        let ordered = |extra: Json| {
            let mut query = json!({ "order": { "field": "id", "direction": "asc" } });
            for (k, v) in extra.as_object().unwrap() {
                query[k] = v.clone();
            }
            q(query)
        };
        let (selector, sort) = sel_sort();
        let doc = |n: usize| json!({ "_id": format!("d{n}"), "_rev": "1-x", "id": format!("d{n}") });

        let (rows, script) = std_twin_rows(&[(200, json!({ "docs": [] }))]);
        rows.query_page("Stock", &ordered(json!({}))).unwrap().unwrap();
        assert_eq!(
            script.sent()[0].2,
            json!({ "selector": selector, "sort": sort, "limit": 1000 }),
            "an unlimited read without an offset sends no skip"
        );

        let (rows, script) = std_twin_rows(&[(200, json!({ "docs": [] })), (200, json!({ "docs": [] }))]);
        rows.query_page("Stock", &ordered(json!({ "offset": 2 }))).unwrap().unwrap();
        let sent = script.sent();
        assert_eq!(
            sent[0].2,
            json!({ "selector": selector, "sort": sort, "skip": 2, "limit": 1000 })
        );
        assert_eq!(
            sent[1].2,
            json!({ "selector": selector, "fields": ["_id"], "limit": 1000 }),
            "an empty page past the offset is counted"
        );

        let (rows, script) = std_twin_rows(&[(200, json!({ "docs": [{ "_id": "d1" }] }))]);
        let ids = rows
            .query_ids("Stock", &ordered(json!({ "limit": 5 })))
            .unwrap()
            .unwrap();
        assert_eq!((ids.ids, ids.total), (vec!["d1".to_string()], 1));
        assert_eq!(
            script.sent()[0].2,
            json!({ "selector": selector, "sort": sort, "fields": ["_id"], "skip": 0, "limit": 5 })
        );

        let (rows, script) = std_twin_rows(&[]);
        let page = rows
            .query_page("Stock", &ordered(json!({ "limit": 0, "total": "none" })))
            .unwrap()
            .unwrap();
        assert_eq!((rows_json(&page.rows), page.total), (json!([]), 0));
        assert!(script.sent().is_empty(), "limit 0 with no total sends nothing");

        let (rows, script) = std_twin_rows(&[(200, json!({ "docs": [doc(1), doc(2)] }))]);
        let page = rows
            .query_page("Stock", &ordered(json!({ "offset": 3, "total": "none" })))
            .unwrap()
            .unwrap();
        assert_eq!(page.total, 2, "a total of none is the number of rows read");
        assert_eq!(script.sent().len(), 1);

        let (rows, script) = std_twin_rows(&[(200, json!({ "docs": [doc(1), doc(2), doc(3)], "bookmark": "b" }))]);
        let page = rows
            .query_page("Stock", &ordered(json!({ "limit": 3, "total": "none" })))
            .unwrap()
            .unwrap();
        assert_eq!(page.total, 3);
        assert_eq!(script.sent().len(), 1, "a full window with no total is not counted");

        let (rows, script) = std_twin_rows(&[(200, json!({ "docs": [] }))]);
        let page = rows
            .query_page("Stock", &ordered(json!({ "limit": 3, "offset": 4, "total": "none" })))
            .unwrap()
            .unwrap();
        assert_eq!(page.total, 0);
        assert_eq!(script.sent().len(), 1, "an empty window past the offset with no total is not counted");
    }

    #[test]
    fn std_twin_guards_refuse_report_the_row_before_and_run_on_the_row_read() {
        let doc = |stock: u32| json!({ "_id": "s1", "_rev": "1-a", "id": "s1", "stock": stock });
        let guarded_update = |guard: Json| {
            std_twin_op(json!({
                "kind": "update", "entityType": "Stock", "id": "s1", "data": { "stock": 0 }, "guard": guard
            }))
        };
        let holds = json!({ "kind": "compare", "field": "stock", "op": ">", "value": 0 });

        let (rows, script) = std_twin_rows(&[(200, doc(1)), (201, json!({ "ok": true }))]);
        match rows.write_where(&guarded_update(holds.clone())).unwrap().unwrap() {
            RowWriteOutcome::Updated { row, before } => {
                assert_eq!(to_json(&row), json!({ "id": "s1", "stock": 0 }));
                assert_eq!(
                    to_json(&before.expect("a guarded update reports the row as it was")),
                    json!({ "id": "s1", "stock": 1 })
                );
            }
            other => panic!("expected an update, got {other:?}"),
        }
        assert_eq!(script.sent()[1].2["_rev"], "1-a");

        let (rows, _) = std_twin_rows(&[(200, doc(1)), (201, json!({ "ok": true }))]);
        let unguarded = std_twin_op(json!({
            "kind": "update", "entityType": "Stock", "id": "s1", "data": { "stock": 0 }, "guard": null
        }));
        assert!(matches!(
            rows.write_where(&unguarded).unwrap().unwrap(),
            RowWriteOutcome::Updated { before: None, .. }
        ));

        let (rows, script) = std_twin_rows(&[(200, doc(0))]);
        assert_eq!(
            rows.write_where(&guarded_update(holds.clone())).unwrap().unwrap(),
            RowWriteOutcome::Refused
        );
        assert_eq!(script.sent().len(), 1, "a refused update writes nothing");

        let delete = |guard: Json| {
            std_twin_op(json!({ "kind": "delete", "entityType": "Stock", "id": "s1", "guard": guard }))
        };
        let (rows, _) = std_twin_rows(&[(200, doc(1)), (200, json!({ "ok": true }))]);
        match rows.write_where(&delete(holds.clone())).unwrap().unwrap() {
            RowWriteOutcome::Deleted { before } => assert_eq!(
                to_json(&before.expect("a guarded delete reports the row as it was")),
                json!({ "id": "s1", "stock": 1 })
            ),
            other => panic!("expected a delete, got {other:?}"),
        }
        let (rows, _) = std_twin_rows(&[(200, doc(0))]);
        assert_eq!(
            rows.write_where(&delete(holds)).unwrap().unwrap(),
            RowWriteOutcome::Refused
        );
        let (rows, _) = std_twin_rows(&[(200, doc(1)), (200, json!({ "ok": true }))]);
        assert_eq!(
            rows.write_where(&delete(Json::Null)).unwrap().unwrap(),
            RowWriteOutcome::Deleted { before: None }
        );

        let row = json!({ "_id": "s1", "_rev": "1-a", "id": "s1", "t": "hello", "tags": ["a", "b"], "n": null, "stock": 1 });
        for (guard, holds) in [
            (json!({ "kind": "contains", "field": "t", "text": "ELL", "caseSensitive": false }), true),
            (json!({ "kind": "contains", "field": "t", "text": "ELL", "caseSensitive": true }), false),
            (json!({ "kind": "hasElement", "field": "tags", "value": "b" }), true),
            (json!({ "kind": "member", "field": "t", "values": ["x", "hello"] }), true),
            (json!({ "kind": "isNull", "field": "n", "isNull": true }), true),
            (json!({ "kind": "not", "of": { "kind": "compare", "field": "t", "op": "<", "value": "a" } }), true),
            (json!({ "kind": "any", "of": [
                { "kind": "const", "value": false },
                { "kind": "compare", "field": "stock", "op": "==", "value": 1 } ] }), true),
            (json!({ "kind": "all", "of": [
                { "kind": "compare", "field": "stock", "op": "==", "value": 1 },
                { "kind": "const", "value": false } ] }), false),
        ] {
            let (rows, _) = std_twin_rows(&[(200, row.clone()), (201, json!({ "ok": true }))]);
            let outcome = rows.write_where(&guarded_update(guard.clone())).unwrap().unwrap();
            assert_eq!(
                matches!(outcome, RowWriteOutcome::Updated { .. }),
                holds,
                "{guard}"
            );
        }
    }

    #[test]
    fn std_twin_a_stop_batch_reports_the_applied_prefix_and_the_failure_and_runs_across_entity_types() {
        let (rows, script) = std_twin_rows(&[
            (201, json!({ "ok": true })),
            (201, json!({ "ok": true })),
            (404, json!({ "error": "not_found" })),
        ]);
        let batch = RowWriteBatch {
            ops: vec![
                std_twin_op(json!({ "kind": "create", "entityType": "Stock", "data": { "id": "s1" } })),
                std_twin_op(json!({ "kind": "delete", "entityType": "TimeEntry", "id": "nope" })),
                std_twin_op(json!({ "kind": "create", "entityType": "Stock", "data": { "id": "s2" } })),
            ],
            on_failure: RowWriteFailureMode::Stop,
        };
        let outcomes = rows.write_batch(&batch).unwrap().unwrap();
        assert_eq!(outcomes.len(), 2);
        assert!(matches!(outcomes[0], RowWriteOutcome::Created { .. }));
        assert_eq!(outcomes[1], RowWriteOutcome::NotFound);
        let sent = script.sent();
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[2].1, "http://c:5984/time_entries/nope");
    }

    #[test]
    fn std_twin_a_create_or_upsert_needs_a_string_id_and_a_row_keeps_nested_values() {
        let (rows, script) = std_twin_rows(&[]);
        for data in [json!({ "stock": 1 }), json!({ "id": 5, "stock": 1 }), json!({ "id": "", "stock": 1 })] {
            let create = RowWriteOp::Create {
                entity_type: "Stock".into(),
                data: from_json(&data),
            };
            assert_eq!(failed(rows.write_where(&create).unwrap().unwrap()), "create needs a string id");
            let upsert = RowWriteOp::Upsert {
                entity_type: "Stock".into(),
                data: from_json(&data),
            };
            assert_eq!(failed(rows.write_where(&upsert).unwrap().unwrap()), "upsert needs a string id");
        }
        assert!(script.sent().is_empty());

        let nested = json!({ "id": "s1", "meta": { "tags": ["a", 1, true, { "k": [1, "x"] }], "deep": { "n": null } }, "mixed": ["a", { "k": 1 }] });
        let (rows, _) = std_twin_rows(&[(
            200,
            json!({ "rows": [{ "id": "s1", "doc": { "_id": "s1", "_rev": "1-a", "id": "s1",
                "meta": nested["meta"].clone(), "mixed": nested["mixed"].clone() } }] }),
        )]);
        let listed: Vec<Value> = rows.list("Stock").unwrap().iter().map(|r| (**r).clone()).collect();
        assert_eq!(rows_json(&listed), json!([nested]));
    }

    #[test]
    fn std_twin_entity_source_read_answers_null_and_create_mints_an_id() {
        let (rows, _) = std_twin_rows(&[(404, json!({ "error": "not_found" }))]);
        assert_eq!(*rows.read("Stock", "gone").unwrap(), Value::Null);
        let (rows, _) = std_twin_rows(&[(201, json!({ "ok": true })), (201, json!({ "ok": true }))]);
        let made = rows.create("Stock", &from_json(&json!({ "stock": 1 }))).unwrap();
        let id = made.as_object().and_then(|o| o.get("id")).and_then(Value::as_str).unwrap().to_string();
        assert_eq!(id.len(), 36, "a UUID v4");
    }
}
