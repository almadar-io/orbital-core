//! The Firestore row store: a kernel store driver over the REST API, reached through the raw http port
//! ([`Transport`]). It is the Rust twin of `@almadar/db/firestore` and shares its layout:
//!
//! - with a `root` document path, rows live at `<root>/entities/<Entity>/rows/<id>`; with an empty root, at the
//!   top-level collection `<Entity>/<id>`; an entity bound to a declared `[persistent: tbl]` collection is keyed by
//!   that collection in place of its name;
//! - a row's document carries its fields and its `id`; a read stamps the document name's id over it.
//!
//! The wire requests are the ones the Firebase Admin SDK sends in REST mode: `batchGet` for a read, `commit` for a
//! write, `runQuery` for a query, `runAggregationQuery` for a count, and a transaction that begins inside its first
//! read (`newTransaction`). The query plan is `@almadar/db`'s `planQuery`; a query Firestore cannot run with the
//! shared two-valued semantics is declined (`query_page` answers `None`) and the kernel reads rows instead.
//! Authentication is the emulator's owner token or a bearer minted from a service account
//! ([`crate::jwt::service_account_assertion`]).

use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::sync::{Arc, Mutex, RwLock};

use indexmap::IndexMap;
use orbital_core_base::codec::encode_uri_component;
use orbital_core_base::row_set::{
    RowCompareOp, RowDirection, RowPredicate, RowQuery, RowTotalMode,
};
use orbital_core_base::value::Value;
use serde_json::{json, Map, Value as Json};

use crate::entity_source::EntitySource;
use crate::http_service::{HttpRequest, Transport};
use crate::jwt::{service_account_assertion, ServiceAccountClaims};
use crate::row_query::{
    apply_write, effective_row_order, query_unsupported_reason, CreateIds, OperatorValueKinds,
    QueryCapabilities, RowCombinator, RowIdPage, RowLeafKind, RowOperator, RowOrderableFields,
    RowPage, RowProjection, RowTable, RowValueKind, RowWriteBatch, RowWriteFailureMode, RowWriteOp,
    RowWriteOutcome, WriteCapabilities,
};

const DEFAULT_PROJECT: &str = "demo-almadar";
const PRODUCTION_BASE: &str = "https://firestore.googleapis.com";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const DATASTORE_SCOPE: &str = "https://www.googleapis.com/auth/datastore";
const DOCUMENT_ID: &str = "__name__";
/// The most values one `in` clause accepts.
pub const IN_LIMIT: usize = 30;
/// A transaction that contention aborts is retried this many times.
const TRANSACTION_ATTEMPTS: usize = 5;

// ---- the Firestore value wire format ----

fn is_whole(f: f64) -> bool {
    f.is_finite() && f.fract() == 0.0 && f.abs() < 9_223_372_036_854_775_000.0
}

/// A JSON number that keeps whole values integral, so `64` stays `64` on the wire.
pub fn number(n: f64) -> Json {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
        Json::Number(serde_json::Number::from(n as i64))
    } else {
        serde_json::Number::from_f64(n).map_or(Json::Null, Json::Number)
    }
}

/// A JSON value as a Firestore `Value`.
pub fn to_wire(value: &Json) -> Json {
    match value {
        Json::Null => json!({ "nullValue": null }),
        Json::Bool(b) => json!({ "booleanValue": b }),
        Json::Number(n) => match n.as_f64() {
            Some(f) if is_whole(f) => json!({ "integerValue": (f as i64).to_string() }),
            Some(f) => json!({ "doubleValue": f }),
            None => json!({ "nullValue": null }),
        },
        Json::String(s) => json!({ "stringValue": s }),
        Json::Array(items) if items.is_empty() => json!({ "arrayValue": {} }),
        Json::Array(items) => {
            json!({ "arrayValue": { "values": items.iter().map(to_wire).collect::<Vec<_>>() } })
        }
        Json::Object(map) => json!({ "mapValue": { "fields": fields_to_wire(map) } }),
    }
}

pub fn fields_to_wire(map: &Map<String, Json>) -> Json {
    Json::Object(
        map.iter()
            .map(|(k, v)| (k.clone(), to_wire(v)))
            .collect::<Map<String, Json>>(),
    )
}

/// A Firestore `Value` as JSON.
pub fn from_wire(value: &Json) -> Json {
    let Some(obj) = value.as_object() else {
        return Json::Null;
    };
    if let Some(s) = obj.get("stringValue").and_then(Json::as_str) {
        return Json::String(s.to_string());
    }
    if let Some(i) = obj.get("integerValue") {
        let parsed = i
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .or_else(|| i.as_f64());
        return parsed.map_or(Json::Null, number);
    }
    if let Some(d) = obj.get("doubleValue").and_then(Json::as_f64) {
        return number(d);
    }
    if let Some(b) = obj.get("booleanValue").and_then(Json::as_bool) {
        return Json::Bool(b);
    }
    if obj.contains_key("nullValue") {
        return Json::Null;
    }
    if let Some(arr) = obj.get("arrayValue") {
        let values = arr.get("values").and_then(Json::as_array);
        return Json::Array(values.map_or_else(Vec::new, |v| v.iter().map(from_wire).collect()));
    }
    if let Some(map) = obj.get("mapValue") {
        let fields = map.get("fields").and_then(Json::as_object);
        return Json::Object(fields.map_or_else(Map::new, fields_from_wire));
    }
    for key in ["timestampValue", "referenceValue"] {
        if let Some(s) = obj.get(key).and_then(Json::as_str) {
            return Json::String(s.to_string());
        }
    }
    Json::Null
}

pub fn fields_from_wire(fields: &Map<String, Json>) -> Map<String, Json> {
    fields
        .iter()
        .map(|(k, v)| (k.clone(), from_wire(v)))
        .collect()
}

const AUTO_ID_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// A Firestore auto-id: 20 characters from `[A-Za-z0-9]`.
pub fn random_auto_id() -> String {
    let state = RandomState::new();
    let mut id = String::with_capacity(20);
    let mut round = 0u64;
    while id.len() < 20 {
        let mut hasher = state.build_hasher();
        hasher.write_u64(round);
        round += 1;
        for byte in hasher.finish().to_le_bytes() {
            if id.len() < 20 && byte <= 247 {
                id.push(AUTO_ID_CHARS[usize::from(byte) % 62] as char);
            }
        }
    }
    id
}

pub(crate) fn to_json(value: &Value) -> Json {
    match value {
        Value::Null | Value::Lambda { .. } => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Number(n) => number(*n),
        Value::String(s) => Json::String(s.clone()),
        Value::Array(items) => Json::Array(items.iter().map(to_json).collect()),
        Value::Object(map) => {
            Json::Object(map.iter().map(|(k, v)| (k.clone(), to_json(v))).collect())
        }
    }
}

pub(crate) fn from_json(value: &Json) -> Value {
    serde_json::from_value(value.clone()).unwrap_or(Value::Null)
}

// ---- connection and authentication ----

/// A bearer token source.
pub type TokenFn = Arc<dyn Fn() -> Result<String, String> + Send + Sync>;

/// How requests authenticate: the emulator accepts any owner token; a project needs a bearer token.
#[derive(Clone)]
pub enum FirestoreAuth {
    Emulator,
    Token(TokenFn),
}

#[derive(Clone)]
pub struct FirestoreConfig {
    pub base: String,
    pub project_id: String,
    pub database_id: String,
    pub auth: FirestoreAuth,
}

impl FirestoreConfig {
    pub fn emulator(host: &str, project_id: &str) -> Self {
        Self {
            base: format!("http://{host}"),
            project_id: project_id.to_string(),
            database_id: "(default)".to_string(),
            auth: FirestoreAuth::Emulator,
        }
    }

    /// The target `@almadar/db/firebase` picks from the environment: `FIRESTORE_EMULATOR_HOST` (the emulator,
    /// project `FIREBASE_PROJECT_ID` or `demo-almadar`), else `FIREBASE_PROJECT_ID` authenticated by a service
    /// account (`FIREBASE_SERVICE_ACCOUNT_PATH`, read through `read_file`, or `FIREBASE_CLIENT_EMAIL` +
    /// `FIREBASE_PRIVATE_KEY`), else a bearer token the host mints (`FIRESTORE_ACCESS_TOKEN`).
    pub fn from_lookup(
        get: &dyn Fn(&str) -> Option<String>,
        read_file: &dyn Fn(&str) -> Result<String, String>,
        transport: Arc<dyn Transport>,
    ) -> Result<Self, String> {
        let database_id = get("FIREBASE_DATABASE_ID")
            .or_else(|| get("FB_DB_ID"))
            .unwrap_or_else(|| "(default)".to_string());
        if let Some(host) = get("FIRESTORE_EMULATOR_HOST") {
            let project = get("FIREBASE_PROJECT_ID").unwrap_or_else(|| DEFAULT_PROJECT.to_string());
            let mut config = Self::emulator(&host, &project);
            config.database_id = database_id;
            return Ok(config);
        }
        let Some(project) = get("FIREBASE_PROJECT_ID") else {
            return Err("@almadar/db/firebase: Cannot initialize Firebase — no credentials found. Set FIREBASE_PROJECT_ID, or FIRESTORE_EMULATOR_HOST.".to_string());
        };
        let key = match (
            get("FIREBASE_SERVICE_ACCOUNT_PATH"),
            get("FIREBASE_CLIENT_EMAIL"),
            get("FIREBASE_PRIVATE_KEY"),
        ) {
            (Some(path), _, _) => {
                let text = read_file(&path)?;
                let file: Json = serde_json::from_str(&text)
                    .map_err(|e| format!("{path} is not a service account key: {e}"))?;
                let field = |name: &str| file.get(name).and_then(Json::as_str).map(str::to_string);
                match (field("client_email"), field("private_key")) {
                    (Some(email), Some(key)) => Some((email, key)),
                    _ => return Err(format!("{path} has no client_email and private_key")),
                }
            }
            (None, Some(email), Some(key)) => Some((email, key.replace("\\n", "\n"))),
            _ => None,
        };
        let auth = match key {
            Some((email, key)) => {
                let source = Arc::new(ServiceAccountToken::new(email, key, transport));
                FirestoreAuth::Token(Arc::new(move || source.access_token()))
            }
            None => {
                let token = get("FIRESTORE_ACCESS_TOKEN");
                FirestoreAuth::Token(Arc::new(move || {
                    token.clone().ok_or_else(|| {
                        "Could not load the default credentials: set FIREBASE_SERVICE_ACCOUNT_PATH, or FIRESTORE_ACCESS_TOKEN".to_string()
                    })
                }))
            }
        };
        Ok(Self {
            base: PRODUCTION_BASE.to_string(),
            project_id: project,
            database_id,
            auth,
        })
    }
}

fn now_secs() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A service account's access token: the RS256 JWT-bearer grant, cached until a minute before it expires.
pub struct ServiceAccountToken {
    email: String,
    private_key: String,
    transport: Arc<dyn Transport>,
    cached: Mutex<Option<(String, u64)>>,
}

impl ServiceAccountToken {
    pub fn new(email: String, private_key: String, transport: Arc<dyn Transport>) -> Self {
        Self {
            email,
            private_key,
            transport,
            cached: Mutex::new(None),
        }
    }

    pub fn access_token(&self) -> Result<String, String> {
        let now = now_secs();
        let mut cached = self
            .cached
            .lock()
            .map_err(|_| "token cache poisoned".to_string())?;
        if let Some((token, expires)) = cached.as_ref() {
            if *expires > now + 60 {
                return Ok(token.clone());
            }
        }
        let assertion = service_account_assertion(
            &ServiceAccountClaims {
                iss: &self.email,
                scope: DATASTORE_SCOPE,
                aud: TOKEN_URL,
                iat: now,
                sub: None,
            },
            &self.private_key,
        )
        .map_err(|e| format!("could not sign the service account assertion: {e:?}"))?;
        let body = format!(
            "grant_type={}&assertion={}",
            encode_uri_component("urn:ietf:params:oauth:grant-type:jwt-bearer"),
            encode_uri_component(&assertion)
        );
        let request = HttpRequest {
            method: "POST".to_string(),
            url: TOKEN_URL.to_string(),
            headers: vec![(
                "content-type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            )],
            body: body.into_bytes(),
            timeout: None,
        };
        let response = self
            .transport
            .send(&request)
            .map_err(|e| format!("{e:?}"))?;
        if !response.is_success() {
            return Err(format!(
                "the token endpoint answered {}: {}",
                response.status,
                response.text()
            ));
        }
        let reply: Json = serde_json::from_str(&response.text())
            .map_err(|e| format!("unreadable token reply: {e}"))?;
        let token = reply
            .get("access_token")
            .and_then(Json::as_str)
            .ok_or_else(|| "the token reply has no access_token".to_string())?
            .to_string();
        let ttl = reply
            .get("expires_in")
            .and_then(Json::as_u64)
            .unwrap_or(3600);
        *cached = Some((token.clone(), now + ttl));
        Ok(token)
    }
}

// ---- the REST client ----

/// What a Firestore reply says went wrong, as the gRPC status word (`FAILED_PRECONDITION`, `ABORTED`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirestoreError {
    pub status: Option<String>,
    pub message: String,
}

impl FirestoreError {
    fn plain(message: impl Into<String>) -> Self {
        Self {
            status: None,
            message: message.into(),
        }
    }

    /// Firestore refuses the query itself (a missing composite index, an unsupported clause mix): the kernel reads
    /// rows instead.
    pub fn declines_a_query(&self) -> bool {
        matches!(
            self.status.as_deref(),
            Some("FAILED_PRECONDITION" | "INVALID_ARGUMENT")
        )
    }

    fn contention(&self) -> bool {
        matches!(self.status.as_deref(), Some("ABORTED"))
    }
}

impl std::fmt::Display for FirestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub type FirestoreResult<T> = Result<T, FirestoreError>;

pub struct BatchGet {
    pub docs: Vec<Option<Map<String, Json>>>,
    pub transaction: Option<String>,
}

pub struct FirestoreClient {
    config: FirestoreConfig,
    transport: Arc<dyn Transport>,
}

impl FirestoreClient {
    pub fn new(config: FirestoreConfig, transport: Arc<dyn Transport>) -> Self {
        Self { config, transport }
    }

    pub fn doc_name(&self, path: &str) -> String {
        format!(
            "projects/{}/databases/{}/documents/{path}",
            self.config.project_id, self.config.database_id
        )
    }

    fn bearer(&self) -> FirestoreResult<String> {
        match &self.config.auth {
            FirestoreAuth::Emulator => Ok("owner".to_string()),
            FirestoreAuth::Token(token) => token().map_err(FirestoreError::plain),
        }
    }

    pub fn post(&self, suffix: &str, body: &Json) -> FirestoreResult<Json> {
        let url = format!(
            "{}/v1/projects/{}/databases/{}/documents{suffix}?$alt=json%3Benum-encoding=int",
            self.config.base, self.config.project_id, self.config.database_id
        );
        let request = HttpRequest {
            method: "POST".to_string(),
            url,
            headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("accept".to_string(), "*/*".to_string()),
                (
                    "authorization".to_string(),
                    format!("Bearer {}", self.bearer()?),
                ),
            ],
            body: body.to_string().into_bytes(),
            timeout: None,
        };
        let response = self
            .transport
            .send(&request)
            .map_err(|e| FirestoreError::plain(format!("{e:?}")))?;
        if !response.is_success() {
            let text = response.text().into_owned();
            let status = serde_json::from_str::<Json>(&text).ok().and_then(|j| {
                j.pointer("/error/status")
                    .and_then(Json::as_str)
                    .map(str::to_string)
            });
            return Err(FirestoreError {
                status,
                message: text,
            });
        }
        serde_json::from_str(&response.text()).map_err(|e| {
            FirestoreError::plain(format!("Firestore answered an unreadable reply: {e}"))
        })
    }

    pub fn batch_get(&self, paths: &[&str], new_transaction: bool) -> FirestoreResult<BatchGet> {
        let mut body =
            json!({ "documents": paths.iter().map(|p| self.doc_name(p)).collect::<Vec<_>>() });
        if new_transaction {
            body["newTransaction"] = json!({ "readWrite": {} });
        }
        let reply = self.post(":batchGet", &body)?;
        let elements = reply.as_array().cloned().unwrap_or_default();
        let mut transaction = None;
        let mut docs: Vec<Option<Map<String, Json>>> = vec![None; paths.len()];
        for element in &elements {
            if let Some(tx) = element.get("transaction").and_then(Json::as_str) {
                transaction = Some(tx.to_string());
            }
            if let Some(found) = element.get("found") {
                let name = found.get("name").and_then(Json::as_str).unwrap_or_default();
                let fields = found
                    .get("fields")
                    .and_then(Json::as_object)
                    .map_or_else(Map::new, fields_from_wire);
                if let Some(at) = paths.iter().position(|p| name == self.doc_name(p)) {
                    docs[at] = Some(fields);
                }
            }
        }
        Ok(BatchGet { docs, transaction })
    }

    pub fn get(&self, path: &str) -> FirestoreResult<Option<Map<String, Json>>> {
        Ok(self.batch_get(&[path], false)?.docs.remove(0))
    }

    pub fn commit(&self, writes: Vec<Json>, transaction: Option<&str>) -> FirestoreResult<()> {
        let mut body = json!({ "writes": writes });
        if let Some(tx) = transaction {
            body["transaction"] = Json::String(tx.to_string());
        }
        self.post(":commit", &body).map(|_| ())
    }

    pub fn set_write(&self, path: &str, data: &Map<String, Json>) -> Json {
        json!({ "update": { "fields": fields_to_wire(data), "name": self.doc_name(path) } })
    }

    pub fn masked_write(&self, path: &str, fields: &Map<String, Json>, must_exist: bool) -> Json {
        let mut paths: Vec<&String> = fields.keys().collect();
        paths.sort();
        let mut write = json!({
            "update": { "fields": fields_to_wire(fields), "name": self.doc_name(path) },
            "updateMask": { "fieldPaths": paths },
        });
        if must_exist {
            write["currentDocument"] = json!({ "exists": true });
        }
        write
    }

    pub fn delete_write(&self, path: &str) -> Json {
        json!({ "delete": self.doc_name(path) })
    }

    pub fn set(&self, path: &str, data: &Map<String, Json>) -> FirestoreResult<()> {
        self.commit(vec![self.set_write(path, data)], None)
    }

    /// `set(data, { merge: true })`.
    pub fn set_merge(&self, path: &str, data: &Map<String, Json>) -> FirestoreResult<()> {
        self.commit(vec![self.masked_write(path, data, false)], None)
    }

    /// `update(fields)`: fails when the document does not exist.
    pub fn update(&self, path: &str, fields: &Map<String, Json>) -> FirestoreResult<()> {
        self.commit(vec![self.masked_write(path, fields, true)], None)
    }

    pub fn create(&self, path: &str, data: &Map<String, Json>) -> FirestoreResult<()> {
        let mut write = self.set_write(path, data);
        write["currentDocument"] = json!({ "exists": false });
        self.commit(vec![write], None)
    }

    pub fn delete(&self, path: &str) -> FirestoreResult<()> {
        self.commit(vec![self.delete_write(path)], None)
    }

    fn query_suffix(parent: &str, verb: &str) -> String {
        if parent.is_empty() {
            format!(":{verb}")
        } else {
            let encoded: Vec<String> = parent.split('/').map(encode_uri_component).collect();
            format!("/{}:{verb}", encoded.join("/"))
        }
    }

    fn structured(collection: &str, extra: &Json) -> Json {
        let mut query = json!({ "from": [{ "collectionId": collection }] });
        if let (Some(target), Some(extra)) = (query.as_object_mut(), extra.as_object()) {
            for (k, v) in extra {
                target.insert(k.clone(), v.clone());
            }
        }
        query
    }

    /// The documents of `collection` under `parent` (`""` for a top-level collection) that match `structured`.
    pub fn query(
        &self,
        parent: &str,
        collection: &str,
        structured: Json,
    ) -> FirestoreResult<Vec<(String, Map<String, Json>)>> {
        let reply = self.post(
            &Self::query_suffix(parent, "runQuery"),
            &json!({ "structuredQuery": Self::structured(collection, &structured) }),
        )?;
        let mut rows = Vec::new();
        for element in reply.as_array().cloned().unwrap_or_default() {
            let Some(document) = element.get("document") else {
                continue;
            };
            let id = document
                .get("name")
                .and_then(Json::as_str)
                .and_then(|n| n.rsplit('/').next())
                .unwrap_or_default()
                .to_string();
            let fields = document
                .get("fields")
                .and_then(Json::as_object)
                .map_or_else(Map::new, fields_from_wire);
            rows.push((id, fields));
        }
        Ok(rows)
    }

    /// How many documents of `collection` match `structured` (its `where`).
    pub fn count(
        &self,
        parent: &str,
        collection: &str,
        structured: Json,
    ) -> FirestoreResult<usize> {
        let reply = self.post(
            &Self::query_suffix(parent, "runAggregationQuery"),
            &json!({ "structuredAggregationQuery": {
                "structuredQuery": Self::structured(collection, &structured),
                "aggregations": [{ "alias": "count", "count": {} }],
            } }),
        )?;
        let count = reply
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|element| element.pointer("/result/aggregateFields/count"))
            .map(from_wire)
            .and_then(|n| n.as_f64())
            .ok_or_else(|| FirestoreError::plain("Firestore answered a count with no result"))?;
        Ok(count as usize)
    }
}

// ---- the query plan ----

/// A native `where` clause.
#[derive(Debug, Clone, PartialEq)]
pub struct Where {
    pub field: String,
    pub op: WhereOp,
    pub value: Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhereOp {
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    ArrayContains,
    ArrayContainsAny,
}

impl WhereOp {
    fn wire(self) -> &'static str {
        match self {
            Self::Eq => "EQUAL",
            Self::Lt => "LESS_THAN",
            Self::Le => "LESS_THAN_OR_EQUAL",
            Self::Gt => "GREATER_THAN",
            Self::Ge => "GREATER_THAN_OR_EQUAL",
            Self::In => "IN",
            Self::ArrayContains => "ARRAY_CONTAINS",
            Self::ArrayContainsAny => "ARRAY_CONTAINS_ANY",
        }
    }

    fn is_range(self) -> bool {
        matches!(self, Self::Lt | Self::Le | Self::Gt | Self::Ge)
    }

    fn array_op(self) -> bool {
        matches!(self, Self::ArrayContains | Self::ArrayContainsAny)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderBy {
    pub field: String,
    pub descending: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plan {
    pub wheres: Vec<Where>,
    pub order_by: Vec<OrderBy>,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
    /// Only the document ids are wanted: the read selects no fields.
    pub ids_only: bool,
    /// No count is read; the total is the window's size.
    pub total_none: bool,
}

/// What planning a query decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Planned {
    /// The query can match nothing: no read is needed.
    Nothing,
    Run(Plan),
    /// Firestore cannot run it with the shared two-valued semantics; the reason is the kernel's fallback word.
    Declined(&'static str),
}

/// What the Firestore plan declares it can run: `==`, ranges, `in`, `array-contains`, `all`/`any` over one field.
pub fn query_capabilities() -> QueryCapabilities {
    let ordered = vec![
        RowValueKind::String,
        RowValueKind::Number,
        RowValueKind::Boolean,
        RowValueKind::Date,
    ];
    QueryCapabilities {
        leaves: vec![
            RowLeafKind::Compare,
            RowLeafKind::Member,
            RowLeafKind::HasElement,
        ],
        combinators: vec![RowCombinator::All, RowCombinator::Any],
        compare_ops: vec![
            RowCompareOp::Eq,
            RowCompareOp::Lt,
            RowCompareOp::Le,
            RowCompareOp::Gt,
            RowCompareOp::Ge,
        ],
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
                value_kinds: ordered.clone(),
            })
            .collect(),
        ),
        orderable_fields: RowOrderableFields::EveryField,
        window: true,
        exact_total: true,
        ids_only: Some(true),
        nearest: None,
    }
}

fn is_scalar(value: &Value) -> bool {
    matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
}

fn field_path(field: &str) -> String {
    if field == "id" {
        DOCUMENT_ID.to_string()
    } else {
        field.to_string()
    }
}

enum Clauses {
    Nothing,
    Some(Vec<Where>),
}

fn declined<T>(reason: &'static str) -> Result<T, &'static str> {
    Err(reason)
}

fn check_in(values: Vec<Json>) -> Result<Vec<Json>, &'static str> {
    if values.len() > IN_LIMIT {
        return declined("value-kind-unsupported");
    }
    Ok(values)
}

fn range_op(op: RowCompareOp) -> Option<WhereOp> {
    match op {
        RowCompareOp::Lt => Some(WhereOp::Lt),
        RowCompareOp::Le => Some(WhereOp::Le),
        RowCompareOp::Gt => Some(WhereOp::Gt),
        RowCompareOp::Ge => Some(WhereOp::Ge),
        RowCompareOp::Eq | RowCompareOp::Ne => None,
    }
}

/// An `any` as one clause: `==`/`member` leaves over one field become `in`, `hasElement` leaves over one field
/// `array-contains-any`.
fn any_clause(children: &[RowPredicate]) -> Result<Clauses, &'static str> {
    let mut field: Option<&str> = None;
    let mut kind: Option<WhereOp> = None;
    let mut values: Vec<Json> = Vec::new();
    for child in children {
        if matches!(child, RowPredicate::Const { value: false }) {
            continue;
        }
        let (child_field, child_kind, child_values): (&str, WhereOp, Vec<Json>) = match child {
            RowPredicate::Compare {
                field,
                op: RowCompareOp::Eq,
                value,
            } => (
                field,
                WhereOp::In,
                if is_scalar(value) {
                    vec![to_json(value)]
                } else {
                    vec![]
                },
            ),
            RowPredicate::Member { field, values } => (
                field,
                WhereOp::In,
                values
                    .iter()
                    .filter(|v| is_scalar(v))
                    .map(to_json)
                    .collect(),
            ),
            RowPredicate::HasElement { field, value } => (
                field,
                WhereOp::ArrayContainsAny,
                if !matches!(value, Value::Null) && is_scalar(value) {
                    vec![to_json(value)]
                } else {
                    vec![]
                },
            ),
            _ => return declined("combinator-unsupported"),
        };
        if field.is_some_and(|f| f != child_field) || kind.is_some_and(|k| k != child_kind) {
            return declined("combinator-unsupported");
        }
        field = Some(child_field);
        kind = Some(child_kind);
        values.extend(child_values);
    }
    match (field, kind) {
        (Some(field), Some(kind)) if !values.is_empty() => Ok(Clauses::Some(vec![Where {
            field: field_path(field),
            op: kind,
            value: Json::Array(check_in(values)?),
        }])),
        _ => Ok(Clauses::Nothing),
    }
}

fn clauses_of(predicate: &RowPredicate) -> Result<Clauses, &'static str> {
    match predicate {
        RowPredicate::Const { value } => Ok(if *value {
            Clauses::Some(vec![])
        } else {
            Clauses::Nothing
        }),
        RowPredicate::Compare { field, op, value } => {
            if !is_scalar(value) {
                return Ok(Clauses::Nothing);
            }
            let op = if *op == RowCompareOp::Eq {
                WhereOp::Eq
            } else if let Some(range) = range_op(*op) {
                range
            } else {
                return declined("leaf-unsupported");
            };
            Ok(Clauses::Some(vec![Where {
                field: field_path(field),
                op,
                value: to_json(value),
            }]))
        }
        RowPredicate::Member { field, values } => {
            let values: Vec<Json> = values
                .iter()
                .filter(|v| is_scalar(v))
                .map(to_json)
                .collect();
            if values.is_empty() {
                return Ok(Clauses::Nothing);
            }
            Ok(Clauses::Some(vec![Where {
                field: field_path(field),
                op: WhereOp::In,
                value: Json::Array(check_in(values)?),
            }]))
        }
        RowPredicate::HasElement { field, value } => {
            if matches!(value, Value::Null) || is_scalar(value) {
                Ok(Clauses::Some(vec![Where {
                    field: field_path(field),
                    op: WhereOp::ArrayContains,
                    value: to_json(value),
                }]))
            } else {
                Ok(Clauses::Nothing)
            }
        }
        RowPredicate::All { of } => {
            let mut out = Vec::new();
            for child in of {
                match clauses_of(child)? {
                    Clauses::Nothing => return Ok(Clauses::Nothing),
                    Clauses::Some(mut clauses) => out.append(&mut clauses),
                }
            }
            Ok(Clauses::Some(out))
        }
        RowPredicate::Any { of } => any_clause(of),
        RowPredicate::Not { .. } | RowPredicate::Contains { .. } | RowPredicate::IsNull { .. } => {
            declined("leaf-unsupported")
        }
    }
}

fn order_bys(
    wheres: &[Where],
    query: &RowQuery,
    orderable_fields: &[String],
) -> Result<Vec<OrderBy>, &'static str> {
    let order = effective_row_order(query);
    let path = field_path(&order.field);
    let descending = order.direction == RowDirection::Desc;
    let required = wheres.iter().any(|w| w.field == path && !w.op.array_op());
    if order.field != "id" && !orderable_fields.contains(&order.field) && !required {
        return declined("order-field");
    }
    let mut inequalities: Vec<&str> = wheres
        .iter()
        .filter(|w| w.op.is_range())
        .map(|w| w.field.as_str())
        .collect();
    inequalities.sort_unstable();
    inequalities.dedup();
    if inequalities.len() > 1 {
        return declined("leaf-unsupported");
    }
    if let Some(inequality) = inequalities.first() {
        if *inequality != path {
            return declined("order-field");
        }
    }
    Ok(if order.field == "id" {
        vec![OrderBy {
            field: DOCUMENT_ID.to_string(),
            descending,
        }]
    } else {
        vec![
            OrderBy {
                field: order.field,
                descending,
            },
            OrderBy {
                field: DOCUMENT_ID.to_string(),
                descending: false,
            },
        ]
    })
}

fn reason_word(reason: crate::row_query::QueryUnsupportedReason) -> &'static str {
    use crate::row_query::QueryUnsupportedReason as R;
    match reason {
        R::LeafUnsupported => "leaf-unsupported",
        R::CombinatorUnsupported => "combinator-unsupported",
        R::ValueKindUnsupported => "value-kind-unsupported",
        R::OrderField => "order-field",
        R::Window => "window",
        R::ExactTotal => "exact-total",
        R::Nearest => "nearest",
    }
}

/// The plan for `query`: `Nothing` when it can match no row, `Declined` when Firestore cannot run it.
pub fn plan_query(query: &RowQuery, orderable_fields: &[String], ids_only: bool) -> Planned {
    if let Some(reason) = query_unsupported_reason(&query_capabilities(), query) {
        return Planned::Declined(reason_word(reason));
    }
    let plan = (|| -> Result<Option<Plan>, &'static str> {
        let mut wheres = Vec::new();
        for predicate in [&query.policy, &query.filter].into_iter().flatten() {
            match clauses_of(predicate)? {
                Clauses::Nothing => return Ok(None),
                Clauses::Some(mut clauses) => wheres.append(&mut clauses),
            }
        }
        let order_by = order_bys(&wheres, query, orderable_fields)?;
        Ok(Some(Plan {
            wheres,
            order_by,
            offset: query.offset,
            limit: query.limit,
            ids_only,
            total_none: query.total == Some(RowTotalMode::None),
        }))
    })();
    match plan {
        Ok(Some(plan)) => Planned::Run(plan),
        Ok(None) => Planned::Nothing,
        Err(reason) => Planned::Declined(reason),
    }
}

/// The match count a windowed read settles by itself: a non-empty window that ended before its limit holds the last
/// match, an empty first page means no match. `None` means the store must count.
pub fn total_from_window(returned: usize, offset: usize, limit: Option<usize>) -> Option<usize> {
    if returned > 0 {
        return match limit {
            None => Some(offset + returned),
            Some(limit) if returned < limit => Some(offset + returned),
            Some(_) => None,
        };
    }
    (offset == 0 && limit != Some(0)).then_some(0)
}

// ---- the row store ----

/// A Firestore-backed row store: the kernel's `EntitySource` over the raw http port.
pub struct FirestoreRows {
    client: FirestoreClient,
    root: String,
    orderable_fields: Vec<String>,
    collections: RwLock<HashMap<String, String>>,
}

fn encode_row(row: &Map<String, Json>) -> Map<String, Json> {
    row.clone()
}

fn row_with_id(id: &str, fields: &Map<String, Json>) -> Value {
    let mut row = fields.clone();
    row.insert("id".to_string(), Json::String(id.to_string()));
    from_json(&Json::Object(row))
}

fn object_of(value: &Value) -> Result<Map<String, Json>, String> {
    match to_json(value) {
        Json::Object(map) => Ok(map),
        other => Err(format!("a row must be an object, got {other}")),
    }
}

fn id_of(value: &Value) -> Option<String> {
    value
        .as_object()
        .and_then(|o| o.get("id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl FirestoreRows {
    /// `root` is a document path the rows hang under (`hosted/<appId>`), or `""` for top-level entity collections.
    /// `orderable_fields` are the fields (besides `id`) every document carries, non-null and indexed with the
    /// document id as tie-break: only these can order a pushed-down query, since Firestore's `orderBy` drops documents
    /// lacking the field.
    pub fn new(
        config: FirestoreConfig,
        transport: Arc<dyn Transport>,
        root: &str,
        orderable_fields: Vec<String>,
    ) -> Result<Self, String> {
        if !root.is_empty() && root.split('/').filter(|s| !s.is_empty()).count() % 2 != 0 {
            return Err(format!(
                "FirestoreRows root \"{root}\" is not a document path (it needs an even number of segments)"
            ));
        }
        Ok(Self {
            client: FirestoreClient::new(config, transport),
            root: root.to_string(),
            orderable_fields,
            collections: RwLock::new(HashMap::new()),
        })
    }

    /// The collection holding an entity's rows, as `@almadar/db/firestore` keys it: the entity's declared
    /// `[persistent: tbl]` collection, else its NAME.
    fn rows_path(&self, entity_type: &str) -> String {
        let key = self
            .collections
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(entity_type)
            .cloned()
            .unwrap_or_else(|| entity_type.to_string());
        if self.root.is_empty() {
            key
        } else {
            format!("{}/entities/{key}/rows", self.root)
        }
    }

    fn split_collection(path: &str) -> (&str, &str) {
        path.rsplit_once('/').unwrap_or(("", path))
    }

    fn doc_path(&self, entity_type: &str, id: &str) -> String {
        format!("{}/{id}", self.rows_path(entity_type))
    }

    fn run_plan(
        &self,
        entity_type: &str,
        plan: &Plan,
    ) -> FirestoreResult<(Vec<(String, Map<String, Json>)>, usize)> {
        let path = self.rows_path(entity_type);
        let (parent, collection) = Self::split_collection(&path);
        let filter = self.where_json(&path, &plan.wheres);
        let counted = |client: &FirestoreClient| client.count(parent, collection, filter.clone());
        if plan.limit == Some(0) {
            let total = if plan.total_none {
                0
            } else {
                counted(&self.client)?
            };
            return Ok((Vec::new(), total));
        }
        let mut structured = filter.clone();
        let object = structured
            .as_object_mut()
            .ok_or_else(|| FirestoreError::plain("filter is not an object"))?;
        if !plan.order_by.is_empty() {
            object.insert(
                "orderBy".to_string(),
                Json::Array(
                    plan.order_by
                        .iter()
                        .map(|o| {
                            json!({
                                "field": { "fieldPath": o.field },
                                "direction": if o.descending { "DESCENDING" } else { "ASCENDING" },
                            })
                        })
                        .collect(),
                ),
            );
        }
        if let Some(offset) = plan.offset.filter(|o| *o > 0) {
            object.insert("offset".to_string(), json!(offset));
        }
        if let Some(limit) = plan.limit {
            object.insert("limit".to_string(), json!(limit));
        }
        if plan.ids_only {
            object.insert(
                "select".to_string(),
                json!({ "fields": [{ "fieldPath": DOCUMENT_ID }] }),
            );
        }
        let docs = self.client.query(parent, collection, structured)?;
        if plan.total_none {
            let total = docs.len();
            return Ok((docs, total));
        }
        let total = match total_from_window(docs.len(), plan.offset.unwrap_or(0), plan.limit) {
            Some(total) => total,
            None => counted(&self.client)?,
        };
        Ok((docs, total))
    }

    /// The `where` of a structured query for `wheres`; a document-id clause compares references.
    fn where_json(&self, rows_path: &str, wheres: &[Where]) -> Json {
        let value = |w: &Where| -> Json {
            if w.field != DOCUMENT_ID {
                return to_wire(&w.value);
            }
            let reference = |id: &Json| json!({ "referenceValue": self.client.doc_name(&format!("{rows_path}/{}", id.as_str().unwrap_or_default())) });
            match &w.value {
                Json::Array(items) => {
                    json!({ "arrayValue": { "values": items.iter().map(reference).collect::<Vec<_>>() } })
                }
                one => reference(one),
            }
        };
        let filters: Vec<Json> = wheres
            .iter()
            .map(|w| json!({ "fieldFilter": { "field": { "fieldPath": w.field }, "op": w.op.wire(), "value": value(w) } }))
            .collect();
        match filters.len() {
            0 => json!({}),
            1 => json!({ "where": filters[0] }),
            _ => json!({ "where": { "compositeFilter": { "op": "AND", "filters": filters } } }),
        }
    }

    fn rows_of(docs: Vec<(String, Map<String, Json>)>) -> Vec<Value> {
        docs.iter()
            .map(|(id, fields)| row_with_id(id, fields))
            .collect()
    }

    /// One transaction over `batch`: every touched document is read first (a Firestore transaction cannot read
    /// after it writes), the ops run over those rows with the core's reference semantics, and the net change is
    /// committed only when the batch may commit. Contention retries the whole transaction.
    fn transact(&self, batch: &RowWriteBatch) -> Result<Vec<RowWriteOutcome>, String> {
        let mut last = String::new();
        for _ in 0..TRANSACTION_ATTEMPTS {
            match self.transact_once(batch) {
                Ok(outcomes) => return Ok(outcomes),
                Err(error) if error.contention() => last = error.message,
                Err(error) => return Err(error.message),
            }
        }
        Err(format!("the Firestore transaction kept aborting: {last}"))
    }

    fn transact_once(&self, batch: &RowWriteBatch) -> FirestoreResult<Vec<RowWriteOutcome>> {
        let mut touched: Vec<(String, String)> = Vec::new();
        for op in &batch.ops {
            let id = match op {
                RowWriteOp::Create { data, .. } | RowWriteOp::Upsert { data, .. } => id_of(data),
                RowWriteOp::Update { id, .. } | RowWriteOp::Delete { id, .. } => Some(id.clone()),
            };
            if let Some(id) = id {
                let key = (op.entity_type().to_string(), id);
                if !touched.contains(&key) {
                    touched.push(key);
                }
            }
        }
        let paths: Vec<String> = touched
            .iter()
            .map(|(entity, id)| self.doc_path(entity, id))
            .collect();
        let (initial, transaction) = if paths.is_empty() {
            (Vec::new(), None)
        } else {
            let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
            let got = self.client.batch_get(&refs, true)?;
            (got.docs, got.transaction)
        };
        let mut tables: IndexMap<String, RowTable> = IndexMap::new();
        for (entity, _) in &touched {
            tables.entry(entity.clone()).or_default();
        }
        for ((entity, id), doc) in touched.iter().zip(&initial) {
            if let Some(fields) = doc {
                if let Some(table) = tables.get_mut(entity) {
                    table.insert(id.clone(), Arc::new(row_with_id(id, fields)));
                }
            }
        }
        let before = tables.clone();
        let mut outcomes: Vec<RowWriteOutcome> = Vec::with_capacity(batch.ops.len());
        for op in &batch.ops {
            let table = tables.entry(op.entity_type().to_string()).or_default();
            let outcome = apply_write(table, op, &mut CreateIds::Strict);
            let applied = outcome.applied();
            outcomes.push(outcome);
            if !applied {
                if batch.on_failure == RowWriteFailureMode::Atomic {
                    let failing = outcomes.pop().unwrap_or(RowWriteOutcome::RolledBack);
                    let mut undone: Vec<RowWriteOutcome> = outcomes
                        .iter()
                        .map(|_| RowWriteOutcome::RolledBack)
                        .collect();
                    undone.push(failing);
                    return Ok(undone);
                }
                break;
            }
        }
        let mut writes: Vec<Json> = Vec::new();
        for (entity, after) in &tables {
            let initial_rows = before.get(entity);
            for id in initial_rows
                .map(|t| t.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
            {
                if !after.contains_key(&id) {
                    writes.push(self.client.delete_write(&self.doc_path(entity, &id)));
                }
            }
            for (id, row) in after {
                let unchanged = initial_rows
                    .and_then(|t| t.get(id))
                    .is_some_and(|prior| **prior == **row);
                if unchanged {
                    continue;
                }
                let mut fields = object_of(row).map_err(FirestoreError::plain)?;
                fields.insert("id".to_string(), Json::String(id.clone()));
                writes.push(
                    self.client
                        .set_write(&self.doc_path(entity, id), &encode_row(&fields)),
                );
            }
        }
        if !writes.is_empty() {
            self.client.commit(writes, transaction.as_deref())?;
        }
        Ok(outcomes)
    }
}

fn str_err(error: FirestoreError) -> String {
    error.message
}

impl EntitySource for FirestoreRows {
    fn read(&self, entity_type: &str, id: &str) -> Result<Arc<Value>, String> {
        Ok(self
            .find(entity_type, id)?
            .unwrap_or_else(|| Arc::new(Value::Null)))
    }

    fn bind_collection(&self, entity_type: &str, collection: &str) {
        self.collections
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(entity_type.to_string(), collection.to_string());
    }

    fn find(&self, entity_type: &str, id: &str) -> Result<Option<Arc<Value>>, String> {
        let doc = self
            .client
            .get(&self.doc_path(entity_type, id))
            .map_err(str_err)?;
        Ok(doc.map(|fields| Arc::new(row_with_id(id, &fields))))
    }

    /// A merge write, creating the row when it is absent; answers the stored row.
    fn write(&self, entity_type: &str, id: &str, data: &Value) -> Result<Value, String> {
        let mut fields = object_of(data)?;
        fields.insert("id".to_string(), Json::String(id.to_string()));
        let path = self.doc_path(entity_type, id);
        self.client
            .set_merge(&path, &encode_row(&fields))
            .map_err(str_err)?;
        let stored = self.client.get(&path).map_err(str_err)?.unwrap_or_default();
        Ok(row_with_id(id, &stored))
    }

    /// A full write of a new row; an id-less row gets a Firestore auto-id.
    fn create(&self, entity_type: &str, data: &Value) -> Result<Value, String> {
        let mut fields = object_of(data)?;
        let id = id_of(data).unwrap_or_else(random_auto_id);
        fields.insert("id".to_string(), Json::String(id.clone()));
        self.client
            .set(&self.doc_path(entity_type, &id), &encode_row(&fields))
            .map_err(str_err)?;
        Ok(row_with_id(&id, &fields))
    }

    fn delete(&self, entity_type: &str, id: &str) -> Result<(), String> {
        self.client
            .delete(&self.doc_path(entity_type, id))
            .map_err(str_err)
    }

    /// A remote store is not cleared by the kernel.
    fn clear(&self) {}

    /// A remote store does not count across entity types.
    fn count(&self) -> usize {
        0
    }

    fn list(&self, entity_type: &str) -> Result<Vec<Arc<Value>>, String> {
        let path = self.rows_path(entity_type);
        let (parent, collection) = Self::split_collection(&path);
        let docs = self
            .client
            .query(parent, collection, json!({}))
            .map_err(str_err)?;
        Ok(Self::rows_of(docs).into_iter().map(Arc::new).collect())
    }

    fn query_page(&self, entity_type: &str, query: &RowQuery) -> Option<Result<RowPage, String>> {
        self.run(entity_type, query, RowProjection::Rows)
            .map(|run| {
                run.map(|(docs, total)| RowPage {
                    rows: Self::rows_of(docs),
                    total,
                })
            })
    }

    fn query_ids(&self, entity_type: &str, query: &RowQuery) -> Option<Result<RowIdPage, String>> {
        self.run(entity_type, query, RowProjection::Ids).map(|run| {
            run.map(|(docs, total)| RowIdPage {
                ids: docs.into_iter().map(|(id, _)| id).collect(),
                total,
            })
        })
    }

    fn write_where(&self, op: &RowWriteOp) -> Option<Result<RowWriteOutcome, String>> {
        let batch = RowWriteBatch {
            ops: vec![op.clone()],
            on_failure: RowWriteFailureMode::Stop,
        };
        Some(self.transact(&batch).and_then(|mut outcomes| {
            outcomes
                .pop()
                .ok_or_else(|| "the Firestore transaction produced no outcome".to_string())
        }))
    }

    fn write_batch(&self, batch: &RowWriteBatch) -> Option<Result<Vec<RowWriteOutcome>, String>> {
        Some(self.transact(batch))
    }
}

impl FirestoreRows {
    /// What a query run answers: `None` declines it (the kernel reads rows), `Some(Err)` is a failure.
    fn run(
        &self,
        entity_type: &str,
        query: &RowQuery,
        projection: RowProjection,
    ) -> Option<Result<(Vec<(String, Map<String, Json>)>, usize), String>> {
        let plan = match plan_query(
            query,
            &self.orderable_fields,
            projection == RowProjection::Ids,
        ) {
            Planned::Nothing => return Some(Ok((Vec::new(), 0))),
            Planned::Declined(_) => return None,
            Planned::Run(plan) => plan,
        };
        match self.run_plan(entity_type, &plan) {
            Ok(done) => Some(Ok(done)),
            Err(error) if error.declines_a_query() => None,
            Err(error) => Some(Err(error.message)),
        }
    }

    /// What the connected store honours for queries: the Firestore plan's capabilities.
    pub fn query_capabilities(&self) -> QueryCapabilities {
        query_capabilities()
    }

    /// Writes run in a transaction, so every write form is honoured.
    pub fn write_capabilities(&self) -> WriteCapabilities {
        WriteCapabilities::full()
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

        fn bodies(&self) -> Vec<Json> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .map(|r| serde_json::from_slice(&r.body).unwrap())
                .collect()
        }

        fn urls(&self) -> Vec<String> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.url.clone())
                .collect()
        }
    }

    impl Transport for Script {
        fn send(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
            self.sent.lock().unwrap().push(request.clone());
            let (status, body) = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| TransportError::Failed("script exhausted".into()))?;
            Ok(HttpResponse {
                status,
                headers: Vec::new(),
                body: body.into_bytes(),
            })
        }
    }

    fn store(
        replies: &[(u16, Json)],
        root: &str,
        orderable: &[&str],
    ) -> (FirestoreRows, Arc<Script>) {
        let script = Script::new(replies);
        let rows = FirestoreRows::new(
            FirestoreConfig::emulator("127.0.0.1:8080", "demo-x"),
            script.clone(),
            root,
            orderable.iter().map(|s| s.to_string()).collect(),
        )
        .unwrap();
        (rows, script)
    }

    fn q(json: Json) -> RowQuery {
        serde_json::from_value(json).unwrap()
    }

    fn plan_of(json: Json, orderable: &[&str]) -> Planned {
        let orderable: Vec<String> = orderable.iter().map(|s| s.to_string()).collect();
        plan_query(&q(json), &orderable, false)
    }

    #[test]
    fn the_wire_value_round_trips() {
        let row = json!({ "a": 1, "b": 1.5, "c": "x", "d": true, "e": null, "f": [1, "y"], "g": { "h": [] } });
        let wire = fields_to_wire(row.as_object().unwrap());
        assert_eq!(wire["a"], json!({ "integerValue": "1" }));
        assert_eq!(wire["b"], json!({ "doubleValue": 1.5 }));
        assert_eq!(
            wire["g"],
            json!({ "mapValue": { "fields": { "h": { "arrayValue": {} } } } })
        );
        assert_eq!(
            Json::Object(fields_from_wire(wire.as_object().unwrap())),
            row
        );
        assert_eq!(random_auto_id().len(), 20);
    }

    #[test]
    fn an_equality_with_an_id_order_plans_a_document_id_sort() {
        let planned = plan_of(
            json!({ "filter": { "kind": "compare", "field": "status", "op": "==", "value": "open" },
                    "order": { "field": "id", "direction": "asc" }, "offset": 2, "limit": 3 }),
            &[],
        );
        let Planned::Run(plan) = planned else {
            panic!("{planned:?}")
        };
        assert_eq!(
            plan.wheres,
            vec![Where {
                field: "status".into(),
                op: WhereOp::Eq,
                value: json!("open")
            }]
        );
        assert_eq!(
            plan.order_by,
            vec![OrderBy {
                field: "__name__".into(),
                descending: false
            }]
        );
        assert_eq!((plan.offset, plan.limit), (Some(2), Some(3)));
    }

    #[test]
    fn a_query_firestore_cannot_run_is_declined_and_an_unmatchable_one_reads_nothing() {
        let order = json!({ "field": "id", "direction": "asc" });
        for filter in [
            json!({ "kind": "compare", "field": "a", "op": "!=", "value": 1 }),
            json!({ "kind": "contains", "field": "a", "text": "x", "caseSensitive": true }),
            json!({ "kind": "isNull", "field": "a", "isNull": true }),
            json!({ "kind": "not", "of": { "kind": "compare", "field": "a", "op": "==", "value": 1 } }),
        ] {
            let planned = plan_of(json!({ "filter": filter, "order": order }), &[]);
            assert!(matches!(planned, Planned::Declined(_)), "{planned:?}");
        }
        assert_eq!(
            plan_of(
                json!({ "policy": { "kind": "const", "value": false } }),
                &[]
            ),
            Planned::Nothing
        );
        assert!(
            matches!(
                plan_of(
                    json!({ "filter": { "kind": "const", "value": true }, "order": order }),
                    &[]
                ),
                Planned::Run(_)
            ),
            "control: a true predicate plans"
        );
    }

    #[test]
    fn an_order_field_that_may_be_missing_is_declined_unless_the_host_guarantees_it() {
        let by = |field: &str| json!({ "order": { "field": field, "direction": "asc" } });
        assert_eq!(plan_of(by("rank"), &[]), Planned::Declined("order-field"));
        let Planned::Run(plan) = plan_of(by("rank"), &["rank"]) else {
            panic!()
        };
        assert_eq!(
            plan.order_by,
            vec![
                OrderBy {
                    field: "rank".into(),
                    descending: false
                },
                OrderBy {
                    field: "__name__".into(),
                    descending: false
                }
            ]
        );
        let ranged = json!({ "filter": { "kind": "compare", "field": "score", "op": ">=", "value": 10 },
                             "order": { "field": "score", "direction": "desc" } });
        assert!(
            matches!(plan_of(ranged, &[]), Planned::Run(_)),
            "a clause on the field requires it"
        );
        let forced = json!({ "filter": { "kind": "compare", "field": "score", "op": ">=", "value": 10 },
                             "order": { "field": "id", "direction": "asc" } });
        assert_eq!(
            plan_of(forced, &[]),
            Planned::Declined("order-field"),
            "an inequality forces the first sort onto its field"
        );
    }

    #[test]
    fn an_any_over_one_field_is_an_in_clause_and_over_two_is_declined() {
        let any = |a: &str, b: &str| {
            json!({ "filter": { "kind": "any", "of": [
                { "kind": "compare", "field": a, "op": "==", "value": 1 },
                { "kind": "compare", "field": b, "op": "==", "value": 2 } ] },
                "order": { "field": "id", "direction": "asc" } })
        };
        let Planned::Run(plan) = plan_of(any("p", "p"), &[]) else {
            panic!()
        };
        assert_eq!(
            plan.wheres,
            vec![Where {
                field: "p".into(),
                op: WhereOp::In,
                value: json!([1, 2])
            }]
        );
        assert_eq!(
            plan_of(any("p", "q"), &[]),
            Planned::Declined("combinator-unsupported")
        );
    }

    #[test]
    fn the_window_settles_the_total_unless_it_is_full() {
        assert_eq!(total_from_window(2, 4, Some(3)), Some(6));
        assert_eq!(total_from_window(3, 4, Some(3)), None);
        assert_eq!(total_from_window(0, 0, None), Some(0));
        assert_eq!(total_from_window(0, 5, None), None);
        assert_eq!(total_from_window(0, 0, Some(0)), None);
    }

    #[test]
    fn a_read_asks_batch_get_for_the_rows_path_and_stamps_the_id() {
        let name = "projects/demo-x/databases/(default)/documents/hosted/app/entities/Task/rows/t1";
        let (rows, script) = store(
            &[(
                200,
                json!([{ "found": { "name": name, "fields": { "title": { "stringValue": "A" } } } }]),
            )],
            "hosted/app",
            &[],
        );
        let row = rows.find("Task", "t1").unwrap().unwrap();
        assert_eq!(to_json(&row), json!({ "title": "A", "id": "t1" }));
        assert_eq!(script.bodies()[0], json!({ "documents": [name] }));
        assert!(
            script.urls()[0].contains("/documents:batchGet"),
            "{:?}",
            script.urls()
        );
        let (rows, _) = store(&[(200, json!([{ "missing": name }]))], "hosted/app", &[]);
        assert!(
            rows.find("Task", "t1").unwrap().is_none(),
            "control: a missing document is no row"
        );
    }

    #[test]
    fn a_declared_collection_keys_the_rows_and_an_undeclared_entity_keeps_its_name() {
        let (rows, script) = store(&[(200, json!([])), (200, json!([]))], "hosted/app", &[]);
        rows.bind_collection("Task", "tasks");
        rows.list("Task").unwrap();
        rows.list("Note").unwrap();
        let (top, top_script) = store(&[(200, json!([])), (200, json!([]))], "", &[]);
        top.bind_collection("Task", "tasks");
        top.list("Task").unwrap();
        top.list("Note").unwrap();
        assert!(
            script.urls()[0].contains("/documents/hosted/app/entities/tasks:runQuery"),
            "{:?}",
            script.urls()
        );
        assert!(
            script.urls()[1].contains("/documents/hosted/app/entities/Note:runQuery"),
            "control: an entity with no declared collection keeps its name {:?}",
            script.urls()
        );
        assert_eq!(
            top_script.bodies()[0]["structuredQuery"]["from"],
            json!([{ "collectionId": "tasks" }])
        );
        assert_eq!(
            top_script.bodies()[1]["structuredQuery"]["from"],
            json!([{ "collectionId": "Note" }]),
            "control: the top-level layout keeps the name when none is declared"
        );
    }

    #[test]
    fn a_top_level_root_keeps_the_entity_collection_and_an_odd_root_is_refused() {
        let (rows, script) = store(&[(200, json!([]))], "", &[]);
        rows.list("Task").unwrap();
        assert_eq!(
            script.bodies()[0]["structuredQuery"]["from"],
            json!([{ "collectionId": "Task" }])
        );
        assert!(
            script.urls()[0].contains("/documents:runQuery"),
            "{:?}",
            script.urls()
        );
        let bad = FirestoreRows::new(
            FirestoreConfig::emulator("h", "p"),
            Script::new(&[]),
            "hosted",
            vec![],
        );
        assert!(bad.is_err(), "control: one segment is not a document path");
    }

    #[test]
    fn a_page_runs_the_plan_and_counts_only_a_full_window() {
        let doc = |id: &str| json!({ "document": { "name": format!("projects/demo-x/databases/(default)/documents/Task/{id}"), "fields": {} } });
        let (rows, script) = store(
            &[
                (200, json!([doc("a"), doc("b")])),
                (
                    200,
                    json!([{ "result": { "aggregateFields": { "count": { "integerValue": "9" } } } }]),
                ),
            ],
            "",
            &[],
        );
        let page = rows
            .query_page(
                "Task",
                &q(json!({ "order": { "field": "id", "direction": "asc" }, "limit": 2 })),
            )
            .unwrap()
            .unwrap();
        assert_eq!(page.total, 9);
        assert_eq!(page.rows.len(), 2);
        let bodies = script.bodies();
        assert_eq!(
            bodies[0]["structuredQuery"]["orderBy"][0]["field"]["fieldPath"],
            "__name__"
        );
        assert_eq!(bodies[0]["structuredQuery"]["limit"], 2);
        assert!(script.urls()[1].contains(":runAggregationQuery"));
        let (rows, script) = store(&[(200, json!([doc("a")]))], "", &[]);
        let page = rows
            .query_page(
                "Task",
                &q(json!({ "order": { "field": "id", "direction": "asc" }, "limit": 5 })),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            page.total, 1,
            "control: a window that ended before its limit needs no count"
        );
        assert_eq!(script.bodies().len(), 1);
    }

    #[test]
    fn a_document_id_comparison_sends_a_reference_value() {
        let (rows, script) = store(&[(200, json!([]))], "", &[]);
        rows.query_ids(
            "Task",
            &q(
                json!({ "filter": { "kind": "compare", "field": "id", "op": ">=", "value": "t10" },
                       "order": { "field": "id", "direction": "asc" } }),
            ),
        )
        .unwrap()
        .unwrap();
        let body = &script.bodies()[0]["structuredQuery"];
        assert_eq!(
            body["where"]["fieldFilter"]["value"],
            json!({ "referenceValue": "projects/demo-x/databases/(default)/documents/Task/t10" })
        );
        assert_eq!(
            body["select"],
            json!({ "fields": [{ "fieldPath": "__name__" }] })
        );
    }

    #[test]
    fn firestore_refusing_a_query_declines_it_and_any_other_failure_stays_one() {
        let refusal = |status: &str| {
            (
                400,
                json!({ "error": { "code": 400, "status": status, "message": "no index" } }),
            )
        };
        let query = q(json!({ "order": { "field": "id", "direction": "asc" } }));
        for status in ["FAILED_PRECONDITION", "INVALID_ARGUMENT"] {
            let (rows, _) = store(&[refusal(status)], "", &[]);
            assert!(
                rows.query_page("Task", &query).is_none(),
                "{status} declines"
            );
        }
        let (rows, _) = store(&[refusal("UNAVAILABLE")], "", &[]);
        assert!(
            rows.query_page("Task", &query).unwrap().is_err(),
            "control: another failure is a failure"
        );
    }

    #[test]
    fn a_write_batch_reads_in_a_transaction_and_commits_the_net_change() {
        let name = |id: &str| format!("projects/demo-x/databases/(default)/documents/Stock/{id}");
        let (rows, script) = store(
            &[
                (
                    200,
                    json!([{ "transaction": "tx1" }, { "found": { "name": name("s1"), "fields": { "stock": { "integerValue": "5" } } } }]),
                ),
                (200, json!({})),
            ],
            "",
            &[],
        );
        let batch: RowWriteBatch = serde_json::from_value(json!({
            "onFailure": "atomic",
            "ops": [{ "kind": "update", "entityType": "Stock", "id": "s1", "data": { "stock": 6 } }]
        }))
        .unwrap();
        let outcomes = rows.write_batch(&batch).unwrap().unwrap();
        assert!(
            matches!(outcomes[0], RowWriteOutcome::Updated { .. }),
            "{outcomes:?}"
        );
        let bodies = script.bodies();
        assert_eq!(bodies[0]["newTransaction"], json!({ "readWrite": {} }));
        assert_eq!(bodies[1]["transaction"], "tx1");
        assert_eq!(
            bodies[1]["writes"][0]["update"]["fields"]["stock"],
            json!({ "integerValue": "6" })
        );
    }

    #[test]
    fn an_atomic_batch_that_fails_commits_nothing_and_an_aborted_transaction_is_retried() {
        let name = "projects/demo-x/databases/(default)/documents/Stock/s1";
        let read = json!([{ "transaction": "tx1" }, { "found": { "name": name, "fields": { "stock": { "integerValue": "5" } } } }]);
        let (rows, script) = store(&[(200, read.clone())], "", &[]);
        let failing: RowWriteBatch = serde_json::from_value(json!({
            "onFailure": "atomic",
            "ops": [
                { "kind": "update", "entityType": "Stock", "id": "s1", "data": { "stock": 6 } },
                { "kind": "update", "entityType": "Stock", "id": "s1", "data": { "stock": 7 },
                  "guard": { "kind": "compare", "field": "stock", "op": "==", "value": 99 } }
            ]
        }))
        .unwrap();
        let outcomes = rows.write_batch(&failing).unwrap().unwrap();
        assert!(
            matches!(outcomes[0], RowWriteOutcome::RolledBack)
                && matches!(outcomes[1], RowWriteOutcome::Refused),
            "{outcomes:?}"
        );
        assert_eq!(script.bodies().len(), 1, "nothing was committed");

        let aborted = (
            409,
            json!({ "error": { "code": 409, "status": "ABORTED", "message": "contention" } }),
        );
        let (rows, script) = store(
            &[(200, read.clone()), aborted, (200, read), (200, json!({}))],
            "",
            &[],
        );
        let one: RowWriteBatch = serde_json::from_value(json!({
            "onFailure": "stop",
            "ops": [{ "kind": "update", "entityType": "Stock", "id": "s1", "data": { "stock": 6 } }]
        }))
        .unwrap();
        let outcomes = rows.write_batch(&one).unwrap().unwrap();
        assert!(
            matches!(outcomes[0], RowWriteOutcome::Updated { .. }),
            "{outcomes:?}"
        );
        assert_eq!(
            script.bodies().len(),
            4,
            "the aborted commit was repeated from its read"
        );
    }

    #[test]
    fn config_from_the_environment_picks_the_emulator_then_a_project() {
        let none = |_: &str| -> Result<String, String> { Err("no file".into()) };
        let env = |pairs: &'static [(&str, &str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            }
        };
        let transport: Arc<dyn Transport> = Script::new(&[]);
        let emulator = FirestoreConfig::from_lookup(
            &env(&[("FIRESTORE_EMULATOR_HOST", "h:1")]),
            &none,
            transport.clone(),
        )
        .unwrap();
        assert_eq!(
            (emulator.base.as_str(), emulator.project_id.as_str()),
            ("http://h:1", DEFAULT_PROJECT)
        );
        let project = FirestoreConfig::from_lookup(
            &env(&[
                ("FIREBASE_PROJECT_ID", "p"),
                ("FIRESTORE_ACCESS_TOKEN", "tok"),
                ("FB_DB_ID", "db2"),
            ]),
            &none,
            transport.clone(),
        )
        .unwrap();
        assert_eq!(
            (project.base.as_str(), project.database_id.as_str()),
            (PRODUCTION_BASE, "db2")
        );
        let FirestoreAuth::Token(token) = &project.auth else {
            panic!("a bearer token")
        };
        assert_eq!(token().unwrap(), "tok");
        assert!(
            FirestoreConfig::from_lookup(&env(&[]), &none, transport).is_err(),
            "control: no credentials is an error"
        );
    }
}
