//! The one sans-I/O driver loop. A protocol driver (TLS) is a pure step machine:
//! it starts with a [`DriverStep`], and each [`DriverAnswer`] resumed into it yields the next. [`drive`] answers
//! every ask through the kernel's journaled [`IoPort`] wrappers, one send or read per ask, so a replay of the
//! journal is a replay of the driver.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::{Deserialize, Serialize};

use crate::embed::wire::Settled;
use crate::resumable::IoPort;
use orbital_core_base::codec;
use orbital_core_base::http_wire::SecretScope;
use orbital_core_base::os::{OsError, OsOp};
use orbital_core_base::secret::{Secret, SecretText};
use orbital_core_base::tab::{TabAsk, TabFailure};
use orbital_core_base::value::Value;

/// What a protocol driver needs its host for; each variant maps onto one journaled `IoPort` wrapper.
#[derive(Clone, PartialEq, Debug)]
pub enum DriverAsk {
    Secret {
        name: String,
        scope: SecretScope,
    },
    SecretSet {
        name: String,
        value: Secret<String>,
        ttl_ms: u64,
        scope: SecretScope,
    },
    /// Replace the host-held credential `name` with `value`, durably (a rotated refresh token).
    SecretRotate {
        name: String,
        value: Secret<String>,
        scope: SecretScope,
    },
    /// `bytes` of host CSPRNG output (journaled by the kernel, so a replay is deterministic).
    Random {
        bytes: u32,
    },
    /// The dispatch's wall clock in epoch milliseconds, journaled.
    Clock,
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
#[derive(Clone, PartialEq, Debug)]
pub enum DriverAnswer {
    Secret(Result<Option<Secret<String>>, String>),
    SecretSet(Result<(), String>),
    SecretRotate(Result<(), String>),
    Random(Result<Secret<Vec<u8>>, String>),
    Clock(Result<i64, String>),
    Os(Result<Value, OsError>),
    Tab(Result<Value, TabFailure>),
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
        value: SecretText,
        ttl_ms: u64,
        #[serde(default, skip_serializing_if = "SecretScope::is_app")]
        scope: SecretScope,
    },
    SecretRotate {
        name: String,
        value: SecretText,
        #[serde(default, skip_serializing_if = "SecretScope::is_app")]
        scope: SecretScope,
    },
    Random {
        bytes: u32,
    },
    Clock,
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
                value: SecretText::new(value.clone()),
                ttl_ms: *ttl_ms,
                scope: *scope,
            },
            DriverAsk::SecretRotate { name, value, scope } => Self::SecretRotate {
                name: name.clone(),
                value: SecretText::new(value.clone()),
                scope: *scope,
            },
            DriverAsk::Random { bytes } => Self::Random { bytes: *bytes },
            DriverAsk::Clock => Self::Clock,
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
                value: value.into_inner(),
                ttl_ms,
                scope,
            },
            DriverAskWire::SecretRotate { name, value, scope } => {
                Self::SecretRotate { name, value: value.into_inner(), scope }
            }
            DriverAskWire::Random { bytes } => Self::Random { bytes },
            DriverAskWire::Clock => Self::Clock,
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
        result: Settled<Option<SecretText>, String>,
    },
    SecretSet {
        result: Settled<(), String>,
    },
    SecretRotate {
        result: Settled<(), String>,
    },
    Random {
        result: Settled<SecretText, String>,
    },
    Clock {
        result: Settled<i64, String>,
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
                result: r.clone().map(|value| value.map(SecretText::new)).into(),
            },
            DriverAnswer::SecretSet(r) => Self::SecretSet {
                result: r.clone().into(),
            },
            DriverAnswer::SecretRotate(r) => Self::SecretRotate {
                result: r.clone().into(),
            },
            DriverAnswer::Clock(r) => Self::Clock {
                result: r.clone().into(),
            },
            DriverAnswer::Random(r) => Self::Random {
                result: r
                    .as_ref()
                    .map(|bytes| SecretText::new(Secret::new(codec::base64_encode(bytes.expose()))))
                    .map_err(Clone::clone)
                    .into(),
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
            DriverAnswerWire::Secret { result } => {
                let result: Result<Option<SecretText>, String> = result.into();
                Self::Secret(result.map(|value| value.map(SecretText::into_inner)))
            }
            DriverAnswerWire::SecretSet { result } => Self::SecretSet(result.into()),
            DriverAnswerWire::SecretRotate { result } => Self::SecretRotate(result.into()),
            DriverAnswerWire::Os { result } => Self::Os(result.into()),
            DriverAnswerWire::Tab { result } => Self::Tab(result.into()),
            DriverAnswerWire::Clock { result } => Self::Clock(result.into()),
            DriverAnswerWire::Random { result } => {
                let result: Result<SecretText, String> = result.into();
                Self::Random(match result {
                    Ok(text) => Ok(Secret::new(
                        codec::base64_decode(text.into_inner().expose())
                            .map_err(|e| format!("random bytes are not base64: {e}"))?,
                    )),
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
}

/// What the loop needs about the call it drives: the signed-in user a user-scoped secret acts
/// for, and the call key an ask is journaled under.
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
                owner(scope).and_then(|who| io.store_secret(&name, value.expose(), ttl_ms, who)),
            ),
            DriverAsk::SecretRotate { name, value, scope } => DriverAnswer::SecretRotate(
                owner(scope).and_then(|who| io.rotate_secret(&name, value.expose(), who)),
            ),
            DriverAsk::Random { bytes } => DriverAnswer::Random(io.random(bytes).map(Secret::new)),
            DriverAsk::Clock => DriverAnswer::Clock(io.clock()),
            DriverAsk::Os { op, args, key } => DriverAnswer::Os(io.os(op, &args, key.as_deref(), None)),
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
        Value::string(error.message.clone())
    }

    impl Toy {
        fn start() -> (Toy, DriverStep) {
            let toy = Toy { phase: Phase::Sent };
            let step = DriverStep::Ask(DriverAsk::Os {
                op: OsOp::Send,
                args: vec![handle(), Value::string("PING")],
                key: None,
            });
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
                    DriverStep::Done(Err(Value::string(format!("unexpected answer {other:?}"))))
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
                site: None, principal: None,
            },
            IoAnswer::Os(answer.map(Secret::new)),
        )
    }

    fn run(journal: Vec<Exchange>) -> (DriveEnd, Vec<IoRequest>) {
        let port = IoPort::new(Arc::new(HostAnswered), None, None);
        let (mut driver, step) = Toy::start();
        let (end, step_end) = port.run_step(journal, || {
            drive(&port, &mut driver, step, DriveContext::default())
        });
        assert!(step_end.diverged.is_none());
        (end, step_end.pending.into_iter().collect())
    }

    fn sent() -> Exchange {
        os_exchange(
            OsOp::Send,
            Value::string("PING"),
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
                Ok(Value::string("PONG")),
            ),
        ];
        let (end, pending) = run(journal);
        assert_eq!(end, DriveEnd::Done(Ok(Value::string("PONG"))));
        assert!(pending.is_empty());
    }

    #[test]
    fn an_unanswered_ask_suspends_with_exactly_one_pending_ask() {
        let (end, pending) = run(Vec::new());
        assert_eq!(end, DriveEnd::AwaitingHost);
        assert!(matches!(
            pending.as_slice(),
            [IoRequest::Os { op: OsOp::Send, .. }]
        ));
        let (end, pending) = run(vec![sent()]);
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
            Value::string("PING"),
            Err(refusal.clone()),
        )];
        let (end, pending) = run(journal);
        assert_eq!(end, DriveEnd::Done(Err(failure(&refusal))));
        assert!(pending.is_empty());
    }
}
