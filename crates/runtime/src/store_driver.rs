//! EXEC-STORE-4 mapping: a row ask on a store-bound entity becomes one typed verb event for its driver
//! orbital (`spec/store/row-store.lolo`), and the driver's keyed reply becomes the same [`IoAnswer`] a host
//! would have given. Mapping only: parking and resuming the asking run is the kernel's.

use crate::resumable::{IoAnswer, IoRequest};
use crate::row_query::{RowIdPage, RowPage, RowProjection, RowWriteBatch, RowWriteOutcome};
use orbital_core_base::json_value::value_to_json;
use orbital_core_base::row_set::RowQuery;
use orbital_core_base::value::Value;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const ROW_READ: &str = "ROW_READ";
pub const ROW_LIST: &str = "ROW_LIST";
pub const ROWS_QUERY: &str = "ROWS_QUERY";
pub const ROWS_WRITE: &str = "ROWS_WRITE";

pub const ROW_FOUND: &str = "ROW_FOUND";
pub const ROWS_LISTED: &str = "ROWS_LISTED";
pub const ROWS_PAGE: &str = "ROWS_PAGE";
pub const ROWS_IDS: &str = "ROWS_IDS";
pub const ROWS_WRITTEN: &str = "ROWS_WRITTEN";
pub const STORE_UNSUPPORTED: &str = "STORE_UNSUPPORTED";
pub const STORE_FAILED: &str = "STORE_FAILED";

/// The verb event a driver orbital receives for one row ask.
#[derive(Debug, Clone, PartialEq)]
pub struct DriverVerb {
    pub event: &'static str,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ReplyError {
    #[error("the driver replied {event} to a {verb} ask")]
    Mismatch { verb: &'static str, event: String },
    #[error("the driver's {event} reply is malformed: {message}")]
    Malformed { event: String, message: String },
    #[error("the ask is not a row ask a store driver answers")]
    NotARowAsk,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadVerb<'a> {
    key: &'a str,
    entity_type: &'a str,
    id: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListVerb<'a> {
    key: &'a str,
    entity_type: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QueryVerb<'a> {
    key: &'a str,
    entity_type: &'a str,
    query: &'a RowQuery,
    project: RowProjection,
}

#[derive(Serialize)]
struct WriteVerb<'a> {
    key: &'a str,
    batch: &'a RowWriteBatch,
}

#[derive(Deserialize)]
struct FoundReply {
    #[serde(default)]
    row: Option<Value>,
}

#[derive(Deserialize)]
struct ListedReply {
    rows: Vec<Value>,
}

#[derive(Deserialize)]
struct WrittenReply {
    outcomes: Vec<RowWriteOutcome>,
}

#[derive(Deserialize)]
struct UnsupportedReply {
    reason: String,
}

#[derive(Deserialize)]
struct FailedReply {
    message: String,
}

fn to_value<T: Serialize>(record: &T) -> Value {
    serde_json::from_value(serde_json::to_value(record).expect("verb records serialize"))
        .expect("serialized verb records are values")
}

/// The verb for `request`, tagged with `key`; `None` for an ask a store driver does not answer. A single-row
/// write reaches a driver as a one-op `WriteRows` batch, never as `CreateRow`/`WriteRow`/`DeleteRow`.
pub fn verb_for(request: &IoRequest, key: &str) -> Option<DriverVerb> {
    let (event, payload) = match request {
        IoRequest::ReadRow { entity_type, id } => (
            ROW_READ,
            to_value(&ReadVerb {
                key,
                entity_type,
                id,
            }),
        ),
        IoRequest::ListRows { entity_type } => (ROW_LIST, to_value(&ListVerb { key, entity_type })),
        IoRequest::QueryRows {
            entity_type,
            query,
            project,
        } => (
            ROWS_QUERY,
            to_value(&QueryVerb {
                key,
                entity_type,
                query,
                project: *project,
            }),
        ),
        IoRequest::WriteRows { batch } => (ROWS_WRITE, to_value(&WriteVerb { key, batch })),
        _ => return None,
    };
    Some(DriverVerb { event, payload })
}

/// The `key` a driver reply carries.
pub fn reply_key(payload: &Value) -> Option<&str> {
    payload.as_object()?.get("key")?.as_str()
}

fn decode<T: for<'de> Deserialize<'de>>(event: &str, payload: &Value) -> Result<T, ReplyError> {
    serde_json::from_value(value_to_json(payload)).map_err(|e| ReplyError::Malformed {
        event: event.to_string(),
        message: e.to_string(),
    })
}

/// The [`IoAnswer`] the driver's reply `event`/`payload` gives `request`: exactly the variant a host would
/// have answered with, so the asking step replays unchanged (EXEC-QUEUE-8/11).
pub fn answer_for(
    request: &IoRequest,
    event: &str,
    payload: &Value,
) -> Result<IoAnswer, ReplyError> {
    let verb = verb_for(request, "").ok_or(ReplyError::NotARowAsk)?.event;
    let failed = || decode::<FailedReply>(event, payload).map(|r| r.message);
    let mismatch = || ReplyError::Mismatch {
        verb,
        event: event.to_string(),
    };
    match (request, event) {
        (_, STORE_FAILED) => {
            let message = failed()?;
            failed_answer(request, message).ok_or(ReplyError::NotARowAsk)
        }
        (IoRequest::QueryRows { .. }, STORE_UNSUPPORTED) => Ok(IoAnswer::QueryUnsupported(
            decode::<UnsupportedReply>(event, payload)?.reason,
        )),
        (IoRequest::WriteRows { .. }, STORE_UNSUPPORTED) => Ok(IoAnswer::WriteUnsupported(
            decode::<UnsupportedReply>(event, payload)?.reason,
        )),
        (IoRequest::ReadRow { .. }, ROW_FOUND) => {
            Ok(IoAnswer::Row(Ok(decode::<FoundReply>(event, payload)?
                .row
                .unwrap_or(Value::Null))))
        }
        (IoRequest::ListRows { .. }, ROWS_LISTED) => {
            Ok(IoAnswer::Rows(Ok(decode::<ListedReply>(event, payload)?
                .rows
                .into_iter()
                .map(Arc::new)
                .collect())))
        }
        (
            IoRequest::QueryRows {
                project: RowProjection::Rows,
                ..
            },
            ROWS_PAGE,
        ) => Ok(IoAnswer::Page(Ok(decode::<RowPage>(event, payload)?))),
        (
            IoRequest::QueryRows {
                project: RowProjection::Ids,
                ..
            },
            ROWS_IDS,
        ) => Ok(IoAnswer::IdPage(Ok(decode::<RowIdPage>(event, payload)?))),
        (IoRequest::WriteRows { .. }, ROWS_WRITTEN) => {
            Ok(IoAnswer::Wrote(Ok(
                decode::<WrittenReply>(event, payload)?.outcomes
            )))
        }
        _ => Err(mismatch()),
    }
}

/// The failed answer of the variant `request` calls for; `None` for an ask that is not a row ask.
pub fn failed_answer(request: &IoRequest, message: String) -> Option<IoAnswer> {
    Some(match request {
        IoRequest::ReadRow { .. } | IoRequest::CreateRow { .. } | IoRequest::WriteRow { .. } => {
            IoAnswer::Row(Err(message))
        }
        IoRequest::ListRows { .. } => IoAnswer::Rows(Err(message)),
        IoRequest::QueryRows { project: RowProjection::Rows, .. } => IoAnswer::Page(Err(message)),
        IoRequest::QueryRows { project: RowProjection::Ids, .. } => IoAnswer::IdPage(Err(message)),
        IoRequest::WriteRows { .. } => IoAnswer::Wrote(Err(message)),
        IoRequest::DeleteRow { .. } => IoAnswer::Deleted(Err(message)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::row_query::{RowWriteFailureMode, RowWriteOp};

    fn json(v: serde_json::Value) -> Value {
        serde_json::from_value(v).unwrap()
    }

    fn read() -> IoRequest {
        IoRequest::ReadRow {
            entity_type: "Ticket".into(),
            id: "t1".into(),
        }
    }

    fn take(guarded: bool) -> IoRequest {
        let guard = guarded.then(|| {
            serde_json::from_value(
                serde_json::json!({"kind": "compare", "field": "used", "op": "==", "value": false}),
            )
            .unwrap()
        });
        IoRequest::WriteRows {
            batch: RowWriteBatch {
                ops: vec![RowWriteOp::Delete {
                    entity_type: "Ticket".into(),
                    id: "t1".into(),
                    guard,
                }],
                on_failure: RowWriteFailureMode::Stop,
            },
        }
    }

    #[test]
    fn a_read_ask_is_a_keyed_row_read_verb() {
        let verb = verb_for(&read(), "k1").unwrap();
        assert_eq!(verb.event, ROW_READ);
        assert_eq!(
            value_to_json(&verb.payload),
            serde_json::json!({"key": "k1", "entityType": "Ticket", "id": "t1"})
        );
    }

    #[test]
    fn a_guarded_take_reaches_the_driver_as_one_conditional_op() {
        let verb = verb_for(&take(true), "k2").unwrap();
        assert_eq!(verb.event, ROWS_WRITE);
        assert_eq!(
            value_to_json(&verb.payload),
            serde_json::json!({"key": "k2", "batch": {"ops": [{"kind": "delete", "entityType": "Ticket", "id": "t1",
                "guard": {"kind": "compare", "field": "used", "op": "==", "value": false}}], "onFailure": "stop"}})
        );
        assert_eq!(
            reply_key(&json(serde_json::json!({"key": "k2", "outcomes": []}))),
            Some("k2")
        );
    }

    #[test]
    fn non_row_asks_and_single_row_writes_have_no_verb() {
        assert!(verb_for(
            &IoRequest::DeleteRow {
                entity_type: "T".into(),
                id: "1".into()
            },
            "k"
        )
        .is_none());
        assert!(verb_for(&IoRequest::CancelCall { key: "k".into() }, "k").is_none());
    }

    #[test]
    fn replies_become_the_answer_a_host_would_give() {
        let found = answer_for(
            &read(),
            ROW_FOUND,
            &json(serde_json::json!({"key": "k", "row": {"id": "t1"}})),
        )
        .unwrap();
        assert_eq!(
            found,
            IoAnswer::Row(Ok(json(serde_json::json!({"id": "t1"}))))
        );
        let absent =
            answer_for(&read(), ROW_FOUND, &json(serde_json::json!({"key": "k"}))).unwrap();
        assert_eq!(absent, IoAnswer::Row(Ok(Value::Null)));
        let gone = answer_for(
            &take(true),
            ROWS_WRITTEN,
            &json(serde_json::json!({"key": "k", "outcomes": [{"kind": "not-found"}]})),
        )
        .unwrap();
        assert_eq!(gone, IoAnswer::Wrote(Ok(vec![RowWriteOutcome::NotFound])));
        let ids_query = IoRequest::QueryRows {
            entity_type: "T".into(),
            query: RowQuery::default(),
            project: RowProjection::Ids,
        };
        let ids = answer_for(
            &ids_query,
            ROWS_IDS,
            &json(serde_json::json!({"key": "k", "ids": ["a"], "total": 3})),
        )
        .unwrap();
        assert_eq!(
            ids,
            IoAnswer::IdPage(Ok(RowIdPage {
                ids: vec!["a".into()],
                total: 3
            }))
        );
    }

    #[test]
    fn unsupported_and_failed_keep_the_host_fallbacks() {
        let q = IoRequest::QueryRows {
            entity_type: "T".into(),
            query: RowQuery::default(),
            project: RowProjection::Rows,
        };
        let r = json(serde_json::json!({"key": "k", "reason": "no push-down"}));
        assert_eq!(
            answer_for(&q, STORE_UNSUPPORTED, &r).unwrap(),
            IoAnswer::QueryUnsupported("no push-down".into())
        );
        assert_eq!(
            answer_for(&take(false), STORE_UNSUPPORTED, &r).unwrap(),
            IoAnswer::WriteUnsupported("no push-down".into())
        );
        let f = json(serde_json::json!({"key": "k", "message": "down"}));
        assert_eq!(
            answer_for(&read(), STORE_FAILED, &f).unwrap(),
            IoAnswer::Row(Err("down".into()))
        );
    }

    #[test]
    fn a_reply_for_another_verb_or_a_malformed_one_is_refused() {
        let wrong = answer_for(
            &read(),
            ROWS_LISTED,
            &json(serde_json::json!({"key": "k", "rows": []})),
        );
        assert!(matches!(
            wrong,
            Err(ReplyError::Mismatch { verb: ROW_READ, .. })
        ));
        assert!(matches!(
            answer_for(
                &read(),
                STORE_UNSUPPORTED,
                &json(serde_json::json!({"key": "k", "reason": "x"}))
            ),
            Err(ReplyError::Mismatch { .. })
        ));
        let bad = answer_for(
            &take(false),
            ROWS_WRITTEN,
            &json(serde_json::json!({"key": "k"})),
        );
        assert!(matches!(bad, Err(ReplyError::Malformed { .. })));
    }
}
