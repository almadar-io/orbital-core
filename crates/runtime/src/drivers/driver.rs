//! The one sans-I/O driver loop. A protocol driver (`http`, then redis, postgres, TLS) is a pure step machine:
//! it starts with a [`DriverStep`], and each [`DriverAnswer`] resumed into it yields the next. [`drive`] answers
//! every ask through the kernel's journaled [`IoPort`] wrappers, one send or read per ask, so a replay of the
//! journal is a replay of the driver.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::{Deserialize, Serialize};

use crate::embed::wire::{HttpFailureWire, HttpRequestWire, HttpResponseWire, Settled};
use crate::http_request::{BeaconOutcome, StreamFraming};
use crate::http_service::{HttpRequest, HttpResponse, TransportError};
use crate::resumable::IoPort;
use orbital_core_base::codec;
use orbital_core_base::http_wire::SecretScope;
use orbital_core_base::oauth_ask::{OAuthAsk, OAuthReply};
use orbital_core_base::os::{OsError, OsOp};
use orbital_core_base::tab::{TabAsk, TabFailure};
use orbital_core_base::value::Value;

/// What a protocol driver needs its host for; each variant maps onto one journaled `IoPort` wrapper.
#[derive(Clone, PartialEq)]
pub enum DriverAsk {
    Secret {
        name: String,
        scope: SecretScope,
    },
    SecretSet {
        name: String,
        value: String,
        ttl_ms: u64,
        scope: SecretScope,
    },
    /// `delay_ms` is how long the host waits before sending (a retry's backoff); `None` sends now.
    Http {
        request: HttpRequest,
        delay_ms: Option<u64>,
    },
    HttpStream {
        request: HttpRequest,
        framing: StreamFraming,
    },
    /// The built request of a beacon call, for the host's fire-and-forget port.
    Beacon {
        request: HttpRequest,
    },
    /// Replace the host-held credential `name` with `value`, durably (a rotated refresh token).
    SecretRotate {
        name: String,
        value: String,
        scope: SecretScope,
    },
    /// `bytes` of host CSPRNG output (journaled by the kernel, so a replay is deterministic).
    Random {
        bytes: u32,
    },
    /// The dispatch's wall clock in epoch milliseconds, journaled.
    Clock,
    /// What an `oauth-client` call needs its host to persist (pending grants, connections, metadata, clock).
    OAuth {
        ask: OAuthAsk,
    },
    /// One `os/<op>` exchange for a protocol driver (`tcp-connect`, `send`, `recv`, ...); `key` lets `cancel-call`
    /// end the ask in flight.
    Os {
        op: OsOp,
        args: Vec<Value>,
        key: Option<String>,
    },
    /// One raw `browser/tab-*` ask (list, open, a page command, ...); the call key lets `cancel-call` end it.
    Tab {
        ask: TabAsk,
    },
}

/// The host's answer to a [`DriverAsk`], one variant per ask.
#[derive(Clone, PartialEq)]
pub enum DriverAnswer {
    Secret(Result<Option<String>, String>),
    SecretSet(Result<(), String>),
    SecretRotate(Result<(), String>),
    Http(Result<HttpResponse, TransportError>),
    Beacon(Result<BeaconOutcome, TransportError>),
    Random(Result<Vec<u8>, String>),
    Clock(Result<i64, String>),
    OAuth(Result<OAuthReply, String>),
    Os(Result<Value, OsError>),
    Tab(Result<Value, TabFailure>),
}

impl std::fmt::Debug for DriverAsk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Secret { name, scope } => f
                .debug_struct("Secret")
                .field("name", name)
                .field("scope", scope)
                .finish(),
            Self::SecretSet {
                name,
                ttl_ms,
                scope,
                ..
            } => f
                .debug_struct("SecretSet")
                .field("name", name)
                .field("value", &"<redacted>")
                .field("ttl_ms", ttl_ms)
                .field("scope", scope)
                .finish(),
            Self::SecretRotate { name, scope, .. } => f
                .debug_struct("SecretRotate")
                .field("name", name)
                .field("value", &"<redacted>")
                .field("scope", scope)
                .finish(),
            Self::Http { request, delay_ms } => f
                .debug_struct("Http")
                .field("request", request)
                .field("delay_ms", delay_ms)
                .finish(),
            Self::HttpStream { request, framing } => f
                .debug_struct("HttpStream")
                .field("request", request)
                .field("framing", framing)
                .finish(),
            Self::Beacon { request } => f.debug_struct("Beacon").field("request", request).finish(),
            Self::Random { bytes } => f.debug_struct("Random").field("bytes", bytes).finish(),
            Self::Clock => f.write_str("Clock"),
            Self::OAuth { ask } => f.debug_struct("OAuth").field("ask", ask).finish(),
            Self::Os { op, args, key } => f
                .debug_struct("Os")
                .field("op", op)
                .field("args", args)
                .field("key", key)
                .finish(),
            Self::Tab { ask } => f.debug_struct("Tab").field("ask", ask).finish(),
        }
    }
}

impl std::fmt::Debug for DriverAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Secret(Ok(Some(_))) => f.write_str("Secret(Ok(Some(<redacted>)))"),
            Self::Secret(other) => f.debug_tuple("Secret").field(other).finish(),
            Self::SecretSet(r) => f.debug_tuple("SecretSet").field(r).finish(),
            Self::SecretRotate(r) => f.debug_tuple("SecretRotate").field(r).finish(),
            Self::Http(r) => f.debug_tuple("Http").field(r).finish(),
            Self::Beacon(r) => f.debug_tuple("Beacon").field(r).finish(),
            Self::Clock(r) => f.debug_tuple("Clock").field(r).finish(),
            Self::Random(Ok(_)) => f.write_str("Random(Ok(<redacted>))"),
            Self::Random(Err(message)) => f
                .debug_tuple("Random")
                .field(&Err::<(), _>(message))
                .finish(),
            Self::OAuth(r) => f.debug_tuple("OAuth").field(r).finish(),
            Self::Os(r) => f.debug_tuple("Os").field(r).finish(),
            Self::Tab(r) => f.debug_tuple("Tab").field(r).finish(),
        }
    }
}

/// What a driver does next: ask its host for one thing, or finish.
#[derive(Debug, Clone, PartialEq)]
pub enum DriverStep {
    Ask(DriverAsk),
    /// `Ok` = success payload, `Err` = failure payload (both carry `key`).
    Done(Result<Value, Value>),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum DriverAskWire {
    Secret {
        name: String,
        #[serde(default, skip_serializing_if = "SecretScope::is_app")]
        scope: SecretScope,
    },
    #[serde(rename_all = "camelCase")]
    SecretSet {
        name: String,
        value: String,
        ttl_ms: u64,
        #[serde(default, skip_serializing_if = "SecretScope::is_app")]
        scope: SecretScope,
    },
    SecretRotate {
        name: String,
        value: String,
        #[serde(default, skip_serializing_if = "SecretScope::is_app")]
        scope: SecretScope,
    },
    #[serde(rename_all = "camelCase")]
    Http {
        request: HttpRequestWire,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delay_ms: Option<u64>,
    },
    HttpStream {
        request: HttpRequestWire,
        framing: StreamFraming,
    },
    Beacon {
        request: HttpRequestWire,
    },
    Random {
        bytes: u32,
    },
    Clock,
    #[serde(rename = "oauth")]
    OAuth {
        ask: OAuthAsk,
    },
    Os {
        op: OsOp,
        args: Vec<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    Tab {
        ask: TabAsk,
    },
}

impl From<&DriverAsk> for DriverAskWire {
    fn from(ask: &DriverAsk) -> Self {
        match ask {
            DriverAsk::Secret { name, scope } => Self::Secret {
                name: name.clone(),
                scope: *scope,
            },
            DriverAsk::SecretSet {
                name,
                value,
                ttl_ms,
                scope,
            } => Self::SecretSet {
                name: name.clone(),
                value: value.clone(),
                ttl_ms: *ttl_ms,
                scope: *scope,
            },
            DriverAsk::SecretRotate { name, value, scope } => Self::SecretRotate {
                name: name.clone(),
                value: value.clone(),
                scope: *scope,
            },
            DriverAsk::Http { request, delay_ms } => Self::Http {
                request: request.into(),
                delay_ms: *delay_ms,
            },
            DriverAsk::HttpStream { request, framing } => Self::HttpStream {
                request: request.into(),
                framing: *framing,
            },
            DriverAsk::Beacon { request } => Self::Beacon {
                request: request.into(),
            },
            DriverAsk::Random { bytes } => Self::Random { bytes: *bytes },
            DriverAsk::Clock => Self::Clock,
            DriverAsk::OAuth { ask } => Self::OAuth { ask: ask.clone() },
            DriverAsk::Os { op, args, key } => Self::Os {
                op: *op,
                args: args.clone(),
                key: key.clone(),
            },
            DriverAsk::Tab { ask } => Self::Tab { ask: ask.clone() },
        }
    }
}

impl From<DriverAskWire> for DriverAsk {
    fn from(wire: DriverAskWire) -> Self {
        match wire {
            DriverAskWire::Secret { name, scope } => Self::Secret { name, scope },
            DriverAskWire::SecretSet {
                name,
                value,
                ttl_ms,
                scope,
            } => Self::SecretSet {
                name,
                value,
                ttl_ms,
                scope,
            },
            DriverAskWire::SecretRotate { name, value, scope } => {
                Self::SecretRotate { name, value, scope }
            }
            DriverAskWire::Http { request, delay_ms } => Self::Http {
                request: request.into(),
                delay_ms,
            },
            DriverAskWire::HttpStream { request, framing } => Self::HttpStream {
                request: request.into(),
                framing,
            },
            DriverAskWire::Beacon { request } => Self::Beacon {
                request: request.into(),
            },
            DriverAskWire::Random { bytes } => Self::Random { bytes },
            DriverAskWire::Clock => Self::Clock,
            DriverAskWire::OAuth { ask } => Self::OAuth { ask },
            DriverAskWire::Os { op, args, key } => Self::Os { op, args, key },
            DriverAskWire::Tab { ask } => Self::Tab { ask },
        }
    }
}

impl Serialize for DriverAsk {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        DriverAskWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DriverAsk {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        DriverAskWire::deserialize(deserializer).map(Self::from)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum DriverAnswerWire {
    Secret {
        result: Settled<Option<String>, String>,
    },
    SecretSet {
        result: Settled<(), String>,
    },
    SecretRotate {
        result: Settled<(), String>,
    },
    Http {
        result: Settled<HttpResponseWire, HttpFailureWire>,
    },
    Beacon {
        result: Settled<BeaconOutcome, HttpFailureWire>,
    },
    Random {
        result: Settled<String, String>,
    },
    Clock {
        result: Settled<i64, String>,
    },
    #[serde(rename = "oauth")]
    OAuth {
        result: Settled<OAuthReply, String>,
    },
    Os {
        result: Settled<Value, OsError>,
    },
    Tab {
        result: Settled<Value, TabFailure>,
    },
}

impl From<&DriverAnswer> for DriverAnswerWire {
    fn from(answer: &DriverAnswer) -> Self {
        match answer {
            DriverAnswer::Secret(r) => Self::Secret {
                result: r.clone().into(),
            },
            DriverAnswer::SecretSet(r) => Self::SecretSet {
                result: r.clone().into(),
            },
            DriverAnswer::SecretRotate(r) => Self::SecretRotate {
                result: r.clone().into(),
            },
            DriverAnswer::Http(r) => Self::Http {
                result: r
                    .as_ref()
                    .map(HttpResponseWire::from)
                    .map_err(HttpFailureWire::from)
                    .into(),
            },
            DriverAnswer::Beacon(r) => Self::Beacon {
                result: r.as_ref().map_err(HttpFailureWire::from).cloned().into(),
            },
            DriverAnswer::Clock(r) => Self::Clock {
                result: r.clone().into(),
            },
            DriverAnswer::Random(r) => Self::Random {
                result: r
                    .as_ref()
                    .map(|bytes| codec::base64_encode(bytes))
                    .map_err(Clone::clone)
                    .into(),
            },
            DriverAnswer::OAuth(r) => Self::OAuth {
                result: r.clone().into(),
            },
            DriverAnswer::Os(r) => Self::Os {
                result: r.clone().into(),
            },
            DriverAnswer::Tab(r) => Self::Tab {
                result: r.clone().into(),
            },
        }
    }
}

impl TryFrom<DriverAnswerWire> for DriverAnswer {
    type Error = String;

    fn try_from(wire: DriverAnswerWire) -> Result<Self, String> {
        Ok(match wire {
            DriverAnswerWire::Secret { result } => Self::Secret(result.into()),
            DriverAnswerWire::SecretSet { result } => Self::SecretSet(result.into()),
            DriverAnswerWire::SecretRotate { result } => Self::SecretRotate(result.into()),
            DriverAnswerWire::Http { result } => {
                let result: Result<HttpResponseWire, HttpFailureWire> = result.into();
                Self::Http(result.map(HttpResponse::from).map_err(TransportError::from))
            }
            DriverAnswerWire::Beacon { result } => {
                let result: Result<BeaconOutcome, HttpFailureWire> = result.into();
                Self::Beacon(result.map_err(TransportError::from))
            }
            DriverAnswerWire::OAuth { result } => Self::OAuth(result.into()),
            DriverAnswerWire::Os { result } => Self::Os(result.into()),
            DriverAnswerWire::Tab { result } => Self::Tab(result.into()),
            DriverAnswerWire::Clock { result } => Self::Clock(result.into()),
            DriverAnswerWire::Random { result } => {
                let result: Result<String, String> = result.into();
                Self::Random(match result {
                    Ok(text) => Ok(codec::base64_decode(&text)
                        .map_err(|e| format!("random bytes are not base64: {e}"))?),
                    Err(message) => Err(message),
                })
            }
        })
    }
}

impl Serialize for DriverAnswer {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        DriverAnswerWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DriverAnswer {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        DriverAnswerWire::deserialize(deserializer)
            .and_then(|wire| Self::try_from(wire).map_err(serde::de::Error::custom))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum DriverStepWire {
    Ask { ask: DriverAsk },
    Done { result: Settled<Value, Value> },
}

impl Serialize for DriverStep {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Ask(ask) => DriverStepWire::Ask { ask: ask.clone() },
            Self::Done(result) => DriverStepWire::Done {
                result: result.clone().into(),
            },
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DriverStep {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match DriverStepWire::deserialize(deserializer)? {
            DriverStepWire::Ask { ask } => Self::Ask(ask),
            DriverStepWire::Done { result } => Self::Done(result.into()),
        })
    }
}

/// A sans-I/O protocol driver: `start` (its constructor) yields the first [`DriverStep`], `resume` the rest.
pub trait Driver {
    fn resume(&mut self, answer: DriverAnswer) -> DriverStep;

    fn http_continuation(&self) -> Option<Box<crate::http_request::HttpCallDriver>> {
        None
    }
}

/// What the loop needs about the call it drives: the signed-in user a user-scoped secret or an OAuth ask acts
/// for, and the call key a http ask is journaled under.
#[derive(Debug, Clone, Copy, Default)]
pub struct DriveContext<'a> {
    pub principal: Option<&'a str>,
    pub key: Option<&'a str>,
}

/// How [`drive`] stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum DriveEnd {
    /// The driver finished: `Ok` is the success payload, `Err` the failure payload.
    Done(Result<Value, Value>),
    /// An ask has no answer yet; the step's result is garbage and the host must answer before it runs again.
    AwaitingHost,
    /// The driver asked for a beacon and no host answers beacons.
    NoBeaconHost,
}

/// Run `driver` from `step` to its end, answering each ask through `io`.
pub(crate) fn drive<D: Driver>(
    io: &IoPort,
    driver: &mut D,
    mut step: DriverStep,
    ctx: DriveContext<'_>,
) -> DriveEnd {
    let owner = |scope: SecretScope| match scope {
        SecretScope::App => Ok(None),
        SecretScope::User => ctx
            .principal
            .map(Some)
            .ok_or_else(|| "nobody is signed in".to_string()),
    };
    loop {
        let ask = match step {
            DriverStep::Done(result) => return DriveEnd::Done(result),
            DriverStep::Ask(ask) => ask,
        };
        let answer = match ask {
            DriverAsk::Secret { name, scope } => {
                DriverAnswer::Secret(owner(scope).and_then(|who| io.secret(&name, who)))
            }
            DriverAsk::SecretSet {
                name,
                value,
                ttl_ms,
                scope,
            } => DriverAnswer::SecretSet(
                owner(scope).and_then(|who| io.store_secret(&name, &value, ttl_ms, who)),
            ),
            DriverAsk::SecretRotate { name, value, scope } => DriverAnswer::SecretRotate(
                owner(scope).and_then(|who| io.rotate_secret(&name, &value, who)),
            ),
            DriverAsk::Random { bytes } => DriverAnswer::Random(io.random(bytes)),
            DriverAsk::Clock => DriverAnswer::Clock(io.clock()),
            DriverAsk::Http { request, delay_ms } => {
                DriverAnswer::Http(io.http_send(&request, ctx.key, delay_ms))
            }
            DriverAsk::HttpStream { request, framing } => {
                DriverAnswer::Http(io.http_stream(&request, ctx.key, framing, driver.http_continuation()))
            }
            DriverAsk::Beacon { request } => match io.http_beacon(&request, ctx.key) {
                Some(outcome) => DriverAnswer::Beacon(outcome),
                None => return DriveEnd::NoBeaconHost,
            },
            DriverAsk::OAuth { ask } => DriverAnswer::OAuth(io.oauth(&ask, ctx.principal)),
            DriverAsk::Os { op, args, key } => DriverAnswer::Os(io.os(op, &args, key.as_deref())),
            DriverAsk::Tab { ask } => DriverAnswer::Tab(io.tab(&ask, ctx.key)),
        };
        if io.suspended() {
            return DriveEnd::AwaitingHost;
        }
        step = driver.resume(answer);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::http_request::tcp_target_allowed;
    use crate::resumable::{Exchange, HostAnswered, IoAnswer, IoRequest};
    use orbital_core_base::os::OsErrorKind;

    #[derive(Debug, PartialEq)]
    enum Phase {
        Sent,
        Received,
    }

    struct Toy {
        phase: Phase,
    }

    fn handle() -> Value {
        Value::Number(3.0)
    }

    fn failure(error: &OsError) -> Value {
        Value::String(error.message.clone())
    }

    impl Toy {
        fn start(target: &str, egress: &[String]) -> (Toy, DriverStep) {
            let toy = Toy { phase: Phase::Sent };
            let step = match tcp_target_allowed(target, egress) {
                Ok(_) => DriverStep::Ask(DriverAsk::Os {
                    op: OsOp::Send,
                    args: vec![handle(), Value::String("PING".to_string())],
                    key: None,
                }),
                Err(refusal) => DriverStep::Done(Err(Value::String(refusal.message()))),
            };
            (toy, step)
        }
    }

    impl Driver for Toy {
        fn resume(&mut self, answer: DriverAnswer) -> DriverStep {
            match (&self.phase, answer) {
                (Phase::Sent, DriverAnswer::Os(Ok(_))) => {
                    self.phase = Phase::Received;
                    DriverStep::Ask(DriverAsk::Os {
                        op: OsOp::Recv,
                        args: vec![handle(), Value::Number(4.0)],
                        key: None,
                    })
                }
                (Phase::Received, DriverAnswer::Os(Ok(reply))) => DriverStep::Done(Ok(reply)),
                (_, DriverAnswer::Os(Err(error))) => DriverStep::Done(Err(failure(&error))),
                (_, other) => {
                    DriverStep::Done(Err(Value::String(format!("unexpected answer {other:?}"))))
                }
            }
        }
    }

    fn os_exchange(op: OsOp, second: Value, answer: Result<Value, OsError>) -> Exchange {
        let args = vec![handle(), second];
        Exchange::new(
            IoRequest::Os {
                op,
                args,
                key: None,
                site: None,
            },
            IoAnswer::Os(answer),
        )
    }

    fn egress() -> Vec<String> {
        vec!["redis://cache.internal:6379".to_string()]
    }

    fn run(journal: Vec<Exchange>, target: &str) -> (DriveEnd, Vec<IoRequest>) {
        let port = IoPort::new(Arc::new(HostAnswered), None, None);
        let (mut driver, step) = Toy::start(target, &egress());
        let (end, step_end) = port.run_step(journal, || {
            drive(&port, &mut driver, step, DriveContext::default())
        });
        assert!(step_end.diverged.is_none());
        (end, step_end.pending.into_iter().collect())
    }

    fn sent() -> Exchange {
        os_exchange(
            OsOp::Send,
            Value::String("PING".to_string()),
            Ok(Value::Null),
        )
    }

    #[test]
    fn a_toy_driver_sends_then_receives_and_finishes() {
        let journal = vec![
            sent(),
            os_exchange(
                OsOp::Recv,
                Value::Number(4.0),
                Ok(Value::String("PONG".to_string())),
            ),
        ];
        let (end, pending) = run(journal, "redis://cache.internal");
        assert_eq!(end, DriveEnd::Done(Ok(Value::String("PONG".to_string()))));
        assert!(pending.is_empty());
    }

    #[test]
    fn an_unanswered_ask_suspends_with_exactly_one_pending_ask() {
        let (end, pending) = run(Vec::new(), "redis://cache.internal");
        assert_eq!(end, DriveEnd::AwaitingHost);
        assert!(matches!(
            pending.as_slice(),
            [IoRequest::Os { op: OsOp::Send, .. }]
        ));
        let (end, pending) = run(vec![sent()], "redis://cache.internal");
        assert_eq!(end, DriveEnd::AwaitingHost);
        assert!(matches!(
            pending.as_slice(),
            [IoRequest::Os { op: OsOp::Recv, .. }]
        ));
    }

    #[test]
    fn control_a_refused_ask_finishes_with_the_failure_value() {
        let refusal = OsError::new(OsErrorKind::Denied, "tcp-send refused");
        let journal = vec![os_exchange(
            OsOp::Send,
            Value::String("PING".to_string()),
            Err(refusal.clone()),
        )];
        let (end, pending) = run(journal, "redis://cache.internal");
        assert_eq!(end, DriveEnd::Done(Err(failure(&refusal))));
        assert!(pending.is_empty());
    }

    #[test]
    fn control_a_target_outside_egress_finishes_without_asking() {
        let (end, pending) = run(Vec::new(), "redis://other.internal:6379");
        assert_eq!(
            end,
            DriveEnd::Done(Err(Value::String(
                "egress denied: redis://other.internal:6379".to_string()
            )))
        );
        assert!(pending.is_empty());
    }

    fn beacon_driver() -> (crate::http_request::HttpCallDriver, DriverStep) {
        let request = Value::Object(orbital_core_base::value::ValueMap::from_iter([(
            "text".to_string(),
            Value::String("hello".to_string()),
        )]));
        let spec = crate::http_request::parse_beacon_call("https://api.example.com/log", &request)
            .expect("beacon spec");
        crate::http_request::HttpCallDriver::start(
            spec,
            vec!["https://api.example.com".to_string()],
            None,
        )
    }

    #[test]
    fn a_beacon_ask_reaches_the_port_and_its_recorded_answer_finishes_the_call() {
        let port = IoPort::new(Arc::new(HostAnswered), None, None);
        let (mut driver, step) = beacon_driver();
        let (end, step_end) = port.run_step(Vec::new(), || {
            drive(&port, &mut driver, step, DriveContext::default())
        });
        assert_eq!(end, DriveEnd::AwaitingHost);
        let [ask @ IoRequest::HttpBeacon { request, .. }] = step_end.pending.as_slice() else {
            panic!("expected one pending beacon ask")
        };
        assert_eq!(request.method, "POST");
        let journal = vec![Exchange::new(
            ask.clone(),
            IoAnswer::HttpBeacon(Ok(crate::http_request::BeaconOutcome::Queued)),
        )];
        let (mut driver, step) = beacon_driver();
        let (end, step_end) = port.run_step(journal, || {
            drive(&port, &mut driver, step, DriveContext::default())
        });
        assert!(matches!(end, DriveEnd::Done(Ok(_))), "{end:?}");
        assert!(step_end.pending.is_empty() && step_end.diverged.is_none());
    }

    #[test]
    fn control_no_beacon_host_ends_in_no_beacon_host() {
        let port = IoPort::new(Arc::new(HostAnswered), None, None);
        let (mut driver, step) = beacon_driver();
        assert_eq!(
            drive(&port, &mut driver, step, DriveContext::default()),
            DriveEnd::NoBeaconHost
        );
    }
}
