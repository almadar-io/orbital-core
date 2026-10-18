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
use parking_lot::Mutex;
use serde_json::{json, Map, Value as Json};

use crate::entity_source::EntitySource;
use crate::firestore::{from_json, to_json, total_from_window};
use crate::http_service::{HttpRequest, Transport};
use crate::pg_rows::{mint_id, table_name_for};
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
            .map_err(|e| format!("{e:?}"))?;
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

    fn db_path(entity_type: &str) -> String {
        format!("/{}", encode_uri_component(&table_name_for(entity_type)))
    }

    fn doc_path(entity_type: &str, id: &str) -> String {
        format!(
            "{}/{}",
            Self::db_path(entity_type),
            encode_uri_component(id)
        )
    }

    fn ensure_database(&self, entity_type: &str) -> Result<(), String> {
        let name = table_name_for(entity_type);
        if self.ensured.lock().contains(&name) {
            return Ok(());
        }
        let reply = self.call("PUT", &Self::db_path(entity_type), None)?;
        // 412 is CouchDB's answer for a database that exists; 409 is accepted as the TS adapter accepts it.
        if !matches!(reply.status, 201 | 202 | 409 | 412) {
            return Self::fail(&reply, "create database");
        }
        self.ensured.lock().insert(name);
        Ok(())
    }

    fn get_doc(&self, entity_type: &str, id: &str) -> Result<Option<Json>, String> {
        let reply = self.call("GET", &Self::doc_path(entity_type, id), None)?;
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
            &Self::doc_path(entity_type, id),
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
            &format!("{}/_find", Self::db_path(entity_type)),
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
                            Self::doc_path(entity_type, id),
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
                Self::doc_path(entity_type, id),
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
            &format!("{}/_all_docs?include_docs=true", Self::db_path(entity_type)),
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
}
