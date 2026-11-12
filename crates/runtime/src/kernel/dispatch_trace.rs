#[cfg(not(feature = "std"))]
use std::prelude::v1::*;
use crate::ProcessEventResult;

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceTransition {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub trait_id: Option<String>,
    pub orbital_id: Option<String>,
    pub source_trait_id: Option<String>,
    pub source_path: Option<String>,
    pub definition_fingerprint: Option<String>,
    pub source_declaration_fingerprint: Option<String>,
    pub observed_state: Option<String>,
    pub from: String,
    pub to: String,
    pub event: Option<String>,
    pub executed: bool,
    pub fired_arm: Option<usize>,
    pub transition_index: Option<usize>,
    pub entity_id: String,
    pub state_entity_id: String,
    pub guards: Vec<String>,
    pub guard_results: Vec<TraceGuard>,
    pub effects: Vec<TraceEffect>,
    pub emits: Vec<TraceEmit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub collapsed: Option<Vec<TraceCollapsed>>,
    pub failure: Option<String>,
}

/// A delivery EXEC-QUEUE-4 skipped as an exact repeat within the dispatch: the trait and event it was for and its payload.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceCollapsed {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub event: String,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub payload: serde_json::Value,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceGuard {
    pub arm: usize,
    pub passed: bool,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub expression: serde_json::Value,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub value: serde_json::Value,
    pub error: Option<String>,
}

/// An `emit` the transition ran, with the payload it carried.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceEmit {
    pub event: String,
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub payload: serde_json::Value,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceEffect {
    #[cfg_attr(test, ts(type = "JsonValue"))]
    pub effect: serde_json::Value,
    pub status: String,
    pub error: Option<String>,
}

pub fn transition_of(
    trait_name: &str,
    result: &ProcessEventResult,
    observed_state: Option<String>,
) -> TraceTransition {
    TraceTransition {
        trait_name: trait_name.to_string(),
        trait_id: None,
        orbital_id: None,
        source_trait_id: None,
        source_path: None,
        definition_fingerprint: None,
        source_declaration_fingerprint: None,
        observed_state,
        from: result.previous_state.clone(),
        to: result.new_state.clone(),
        event: result.event.clone(),
        executed: result.executed,
        fired_arm: result.fired_arm,
        transition_index: result.transition_index,
        entity_id: crate::process_result::public_entity_id(&result.entity_id).to_string(),
        state_entity_id: crate::process_result::public_entity_id(&result.state_entity_id).to_string(),
        guards: result.guards.iter().map(|g| format!("{g:?}")).collect(),
        guard_results: result
            .guards
            .iter()
            .map(|g| TraceGuard {
                arm: g.arm,
                passed: g.guard.passed,
                expression: serde_json::to_value(g.guard.guard.as_ref())
                    .expect("guard expression serializes"),
                value: serde_json::to_value(&g.guard.value).expect("guard value serializes"),
                error: g.guard.error.clone(),
            })
            .collect(),
        effects: result
            .effect_runs
            .iter()
            .map(|run| TraceEffect {
                effect: serde_json::to_value(&run.effect).unwrap_or(serde_json::Value::Null),
                status: format!("{:?}", run.status),
                error: run.error.clone(),
            })
            .collect(),
        emits: result
            .effect_results
            .iter()
            .filter_map(|run| match &run.effect {
                orbital_core_base::effect_types::Effect::Emit { event, payload } => {
                    Some(TraceEmit {
                        event: event.clone(),
                        payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
                    })
                }
                _ => None,
            })
            .collect(),
        collapsed: (!result.collapsed.is_empty()).then(|| {
            result
                .collapsed
                .iter()
                .map(|c| TraceCollapsed {
                    trait_name: c.trait_name.clone(),
                    event: c.event.clone(),
                    payload: serde_json::to_value(&*c.payload).unwrap_or(serde_json::Value::Null),
                })
                .collect()
        }),
        failure: result.failure.clone(),
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceSeed {
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub entity_id: Option<String>,
    pub event: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchTraceRecord {
    pub dispatch_id: u64,
    pub seeds: Vec<TraceSeed>,
    pub outcomes: Vec<TraceTransition>,
    pub error: Option<String>,
}

/// Runs before the dispatch lane releases; observers must not dispatch into the kernel.
pub struct DispatchObserver {
    pub enabled: Box<dyn Fn() -> bool + Send + Sync>,
    pub completed: Box<dyn Fn(DispatchTraceRecord) + Send + Sync>,
}

impl DispatchTraceRecord {
    /// What started the dispatch: its seeds as `Trait.EVENT` joined by `+`, the first seed's row (`singleton` when it
    /// has none) and the seed traits.
    pub fn seed_summary(&self) -> (String, String, Vec<String>) {
        let seed = self
            .seeds
            .iter()
            .map(|s| format!("{}.{}", s.trait_name, s.event))
            .collect::<Vec<_>>()
            .join("+");
        let entity = self
            .seeds
            .first()
            .and_then(|s| s.entity_id.clone())
            .unwrap_or_else(|| "singleton".into());
        (seed, entity, self.seeds.iter().map(|s| s.trait_name.clone()).collect())
    }
}

/// The trace a kernel keeps for a host with no journal of its own (the browser): each dispatch, once on, as the
/// item a `program/live { kind: "trace" }` stream yields (EXEC-DIST-14), until the host takes them.
#[derive(Default)]
pub struct TraceFeed {
    on: core::sync::atomic::AtomicBool,
    seq: core::sync::atomic::AtomicU64,
    items: orbital_core_base::sync::Mutex<Vec<serde_json::Value>>,
}

impl TraceFeed {
    /// The observer that records into this feed while it is on.
    pub fn observer(self: &std::sync::Arc<Self>) -> DispatchObserver {
        let enabled = std::sync::Arc::clone(self);
        let completed = std::sync::Arc::clone(self);
        DispatchObserver {
            enabled: Box::new(move || enabled.on.load(core::sync::atomic::Ordering::SeqCst)),
            completed: Box::new(move |record| completed.record(&record)),
        }
    }

    /// Start or stop recording; stopping drops what the host has not taken.
    pub fn set_on(&self, on: bool) {
        self.on.store(on, core::sync::atomic::Ordering::SeqCst);
        if !on {
            self.items.lock().clear();
        }
    }

    /// The items recorded since the last take, in dispatch order.
    pub fn take(&self) -> Vec<serde_json::Value> {
        core::mem::take(&mut *self.items.lock())
    }

    fn record(&self, record: &DispatchTraceRecord) {
        let (seed, seed_entity_id, seed_traits) = record.seed_summary();
        let item = serde_json::json!({
            "type": "trace",
            "dispatch": {
                "seq": self.seq.fetch_add(1, core::sync::atomic::Ordering::SeqCst),
                "dispatchId": record.dispatch_id,
                "seed": seed,
                "seedEntityId": seed_entity_id,
                "seedTraits": seed_traits,
                "outcomes": record.outcomes,
                "error": record.error,
            }
        });
        self.items.lock().push(item);
    }
}

orbital_core_base::kernel_local! {
    static ANSWER_DISPATCH: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

pub fn answer_dispatch_id() -> Option<u64> {
    ANSWER_DISPATCH.with(std::cell::Cell::get)
}

pub(crate) struct AnswerContext(Option<u64>);

impl AnswerContext {
    pub(crate) fn enter(dispatch_id: Option<u64>) -> Self {
        Self(ANSWER_DISPATCH.with(|current| current.replace(dispatch_id)))
    }
}

impl Drop for AnswerContext {
    fn drop(&mut self) {
        ANSWER_DISPATCH.with(|current| current.set(self.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resumable::{answer_batch, IoAnswer, IoRequest, PendingRequest};
    use orbital_core_base::value::Value;

    fn record(trait_name: &str, event: &str) -> DispatchTraceRecord {
        DispatchTraceRecord {
            dispatch_id: 3,
            seeds: vec![TraceSeed { trait_name: trait_name.into(), entity_id: None, event: event.into() }],
            outcomes: vec![],
            error: None,
        }
    }

    #[test]
    fn dispatch_trace_feed_records_only_while_on_and_hands_each_item_once_in_order() {
        let feed = std::sync::Arc::new(TraceFeed::default());
        let observer = feed.observer();
        assert!(!(observer.enabled)(), "off until a host turns it on");
        feed.set_on(true);
        assert!((observer.enabled)());
        (observer.completed)(record("Board", "OPEN"));
        (observer.completed)(record("Board", "CLOSE"));
        let items = feed.take();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["type"], "trace");
        assert_eq!((items[0]["dispatch"]["seq"].clone(), items[0]["dispatch"]["seed"].clone()), (serde_json::json!(0), serde_json::json!("Board.OPEN")));
        assert_eq!((items[1]["dispatch"]["seq"].clone(), items[1]["dispatch"]["seedEntityId"].clone()), (serde_json::json!(1), serde_json::json!("singleton")));
        assert!(feed.take().is_empty(), "each item is handed once");
        (observer.completed)(record("Board", "OPEN"));
        feed.set_on(false);
        assert!(feed.take().is_empty(), "stopping drops what was not taken");
    }

    #[test]
    fn dispatch_trace_answer_context_restores_the_outer_dispatch_after_nested_answers() {
        assert_eq!(answer_dispatch_id(), None);
        let outer = AnswerContext::enter(Some(7));
        {
            let _inner = AnswerContext::enter(Some(9));
            assert_eq!(answer_dispatch_id(), Some(9));
        }
        assert_eq!(answer_dispatch_id(), Some(7));
        drop(outer);
        assert_eq!(answer_dispatch_id(), None);
    }

    #[test]
    fn dispatch_trace_answer_context_follows_keyed_workers_and_inline_answers() {
        let outer = AnswerContext::enter(Some(41));
        let requests = [
            PendingRequest {
                id: crate::resumable::RequestId::from_raw(1),
                request: IoRequest::CallService {
                    service: "fixture".into(),
                    method: "read".into(),
                    args: Value::Null,
                    key: Some("call".into()),
                    caller: None,
                    site: None,
                },
            },
            PendingRequest {
                id: crate::resumable::RequestId::from_raw(2),
                request: IoRequest::ListRows {
                    entity_type: "Fixture".into(),
                },
            },
        ];
        let replies = answer_batch(&requests, &|_| {
            assert_eq!(answer_dispatch_id(), Some(41));
            IoAnswer::Service(Ok(Value::Null))
        });
        assert_eq!(replies.len(), 2);
        assert_eq!(answer_dispatch_id(), Some(41));
        drop(outer);
        assert_eq!(answer_dispatch_id(), None);
    }
}
