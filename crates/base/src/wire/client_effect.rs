//! Client-effect wire grammar — the Rust mirror of `@almadar/core`'s
//! `ClientEffectTuple` (`packages/almadar-core/src/types/bus.ts`).
//!
//! The client only ever receives `render-ui` / `navigate` / `navigate-back` / `host-ask` on this
//! channel (server-only effects report through `ServerEffectResult`, and `emit`
//! surfaces as `EmittedEvent` — see `effects::response`). The wire shape is a JSON
//! ARRAY whose arity carries which trailing fields are present, so this is the one
//! type in the crate with a hand-written `Serialize`/`Deserialize` instead of a
//! derive — no other shape in the wire needs positional (rather than keyed) framing.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::value::Value;
use serde::de::{Error as DeError, SeqAccess, Visitor};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

use crate::effect_types::{Effect, EffectOutcome, EffectResult};

/// `{ crumb?: string }` — the optional fourth element of a `navigate` tuple.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NavigateOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crumb: Option<String>,
}

/// Client-side effect tuple shipped in `OrbitalEventResponse.clientEffects`. Mirrors
/// `@almadar/core`'s `ClientEffectTuple` union (`bus.ts`) byte-for-byte, arity by
/// field presence — never a 7-variant untagged enum (Coordinator decision,
/// `groovy-inventing-cloud.md`).
#[derive(Debug, Clone, PartialEq)]
pub enum ClientEffect {
    /// `["render-ui", slot, pattern]`: `render-ui` takes a slot and a pattern (G-CROSS-257).
    RenderUi {
        slot: String,
        /// `PatternConfig | null` — always present on the wire, `null` when absent.
        config: Option<Value>,
    },
    Navigate {
        path: String,
        params: Option<Value>,
        options: Option<NavigateOptions>,
    },
    NavigateBack,
    /// `["host-ask", op, [args...]]`: a `runsOn: client` host-port effect (`auth/sign-out`, `audio/*`) the client host
    /// answers, its arguments resolved by the kernel.
    HostAsk {
        op: String,
        args: Vec<Value>,
        /// Where the host returns its answer; `None` for an ask that routes none (`auth/sign-out`, `audio/*`).
        completion: Option<HostAskCompletion>,
    },
}

/// The return address of a `host-ask` that declared `emit` routes (EXEC-DIST-13): the trait that asked, the frame it
/// asked on when that frame is a row, the call's `key`, and the events its two outcomes fire. The host answers once
/// by dispatching the event of the outcome it reached to `trait_name`; a route the ask did not declare is absent and
/// that outcome fires nothing.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HostAskCompletion {
    #[serde(rename = "trait")]
    pub trait_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

impl HostAskCompletion {
    /// The completion a host-asked `effect` declares on `trait_name`'s `entity` frame: its resolved `key` and its
    /// `emit` routes. `None` when it declares neither.
    pub fn of(effect: &Effect, trait_name: &str, entity: Option<&str>) -> Option<Self> {
        let Effect::Custom { key, emit, .. } = effect else {
            return None;
        };
        let key = key.as_ref().and_then(Value::as_str).map(str::to_string);
        let success = emit.as_ref().and_then(|e| e.success.clone());
        let failure = emit.as_ref().and_then(|e| e.failure.clone());
        if success.is_none() && failure.is_none() {
            return None;
        }
        Some(Self {
            trait_name: trait_name.to_string(),
            entity: entity.map(str::to_string),
            key,
            success,
            failure,
        })
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let text = |field: &str| -> Result<Option<String>, String> {
            match value.get_path(field) {
                None => Ok(None),
                Some(v) => v
                    .as_str()
                    .map(|s| Some(s.to_string()))
                    .ok_or_else(|| format!("host-ask tuple: completion {field} must be a string")),
            }
        };
        let trait_name = text("trait")?
            .ok_or_else(|| "host-ask tuple: completion needs the asking trait".to_string())?;
        Ok(Self {
            trait_name,
            entity: text("entity")?,
            key: text("key")?,
            success: text("success")?,
            failure: text("failure")?,
        })
    }
}

impl ClientEffect {
    /// Convert a resolved internal [`Effect`] to its wire tuple, if the effect is
    /// client-targeted in tuple form (`render-ui`/`navigate`/`navigate-back`; `emit`
    /// surfaces separately as an [`super::EmittedEvent`], never here).
    ///
    /// For `RenderUi`, the pattern object a renderer parses (an empty object for a `null` pattern).
    pub fn render_ui_value(&self) -> Option<Value> {
        let ClientEffect::RenderUi { config, .. } = self else {
            return None;
        };
        Some(
            config
                .clone()
                .unwrap_or_else(|| Value::Object(Default::default())),
        )
    }

    /// The wire tuple of an effect a dispatch ran: [`Self::from_effect`], or the `host-ask` of an effect the client
    /// handed to its host ([`EffectResult::host_asked`]).
    pub fn from_result(result: &EffectResult) -> Option<ClientEffect> {
        Self::from_result_in(result, None)
    }

    /// [`Self::from_result`] for an effect `trait_name` ran on `entity` (its frame when that is a row): a host ask
    /// that declared `emit` routes carries them as its return address.
    pub fn from_result_in(result: &EffectResult, site: Option<(&str, Option<&str>)>) -> Option<ClientEffect> {
        match (&result.outcome, &result.effect) {
            (EffectOutcome::HostAsked, Effect::Custom { name, args, .. }) => Some(ClientEffect::HostAsk {
                op: name.clone(),
                args: args.clone(),
                completion: site.and_then(|(trait_name, entity)| HostAskCompletion::of(&result.effect, trait_name, entity)),
            }),
            _ => Self::from_effect(&result.effect),
        }
    }

    /// Convert a resolved internal [`Effect`] to its wire tuple, if the effect is client-targeted in tuple form
    /// (`render-ui`/`navigate`/`navigate-back`; `emit` surfaces separately as an [`super::EmittedEvent`]). The
    /// kernel resolves a `render-ui`'s bindings once, so its whole bound content is the pattern.
    pub fn from_effect(effect: &Effect) -> Option<ClientEffect> {
        match effect {
            Effect::RenderUi { slot, content } => Some(ClientEffect::RenderUi {
                slot: slot.clone(),
                config: if content.is_null() {
                    None
                } else {
                    Some(content.clone())
                },
            }),
            Effect::Navigate {
                path,
                params,
                options,
            } => Some(ClientEffect::Navigate {
                path: path.clone(),
                params: params.as_ref().map(|p| Value::object(p.clone())),
                options: options.clone(),
            }),
            Effect::NavigateBack => Some(ClientEffect::NavigateBack),
            _ => None,
        }
    }
}

/// `Some(value)` serializes as itself; `None` serializes as JSON `null` — the array
/// grammar has no "absent slot", only a present-but-null one, once a later element
/// forces the tuple to that arity.
fn serialize_opt_value<S: SerializeSeq>(
    seq: &mut S,
    value: Option<&Value>,
) -> Result<(), S::Error> {
    match value {
        Some(v) => seq.serialize_element(v),
        None => seq.serialize_element(&Value::Null),
    }
}

impl Serialize for ClientEffect {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            ClientEffect::RenderUi { slot, config } => {
                let mut seq = serializer.serialize_seq(Some(3))?;
                seq.serialize_element("render-ui")?;
                seq.serialize_element(slot)?;
                serialize_opt_value(&mut seq, config.as_ref())?;
                seq.end()
            }
            ClientEffect::Navigate {
                path,
                params,
                options,
            } => {
                let len = if options.is_some() {
                    4
                } else if params.is_some() {
                    3
                } else {
                    2
                };
                let mut seq = serializer.serialize_seq(Some(len))?;
                seq.serialize_element("navigate")?;
                seq.serialize_element(path)?;
                if len >= 3 {
                    serialize_opt_value(&mut seq, params.as_ref())?;
                }
                if len == 4 {
                    seq.serialize_element(options.as_ref().unwrap())?;
                }
                seq.end()
            }
            ClientEffect::NavigateBack => {
                let mut seq = serializer.serialize_seq(Some(1))?;
                seq.serialize_element("navigate-back")?;
                seq.end()
            }
            ClientEffect::HostAsk { op, args, completion } => {
                let mut seq = serializer.serialize_seq(Some(if completion.is_some() { 4 } else { 3 }))?;
                seq.serialize_element("host-ask")?;
                seq.serialize_element(op)?;
                seq.serialize_element(args)?;
                if let Some(completion) = completion {
                    seq.serialize_element(completion)?;
                }
                seq.end()
            }
        }
    }
}

struct ClientEffectVisitor;

impl<'de> Visitor<'de> for ClientEffectVisitor {
    type Value = ClientEffect;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a client-effect tuple: [\"render-ui\"|\"navigate\"|\"navigate-back\"|\"host-ask\", ...]")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut items: Vec<Value> = Vec::new();
        while let Some(v) = seq.next_element::<Value>()? {
            items.push(v);
        }
        let tag = items
            .first()
            .and_then(Value::as_str)
            .ok_or_else(|| DeError::custom("client-effect tuple: missing tag"))?
            .to_string();

        let opt = |v: Option<&Value>| -> Option<Value> { v.filter(|x| !x.is_null()).cloned() };

        match tag.as_str() {
            "render-ui" => {
                if items.len() != 3 {
                    return Err(DeError::custom(
                        "render-ui tuple takes a slot and a pattern: [\"render-ui\", slot, pattern]",
                    ));
                }
                let slot = items[1]
                    .as_str()
                    .ok_or_else(|| DeError::custom("render-ui tuple: slot must be a string"))?
                    .to_string();
                Ok(ClientEffect::RenderUi {
                    slot,
                    config: opt(items.get(2)),
                })
            }
            "navigate" => {
                if items.len() < 2 {
                    return Err(DeError::custom(
                        "navigate tuple needs at least [\"navigate\", path]",
                    ));
                }
                let path = items[1]
                    .as_str()
                    .ok_or_else(|| DeError::custom("navigate tuple: path must be a string"))?
                    .to_string();
                let params = opt(items.get(2));
                let options = match items.get(3) {
                    Some(v) if !v.is_null() => Some(NavigateOptions {
                        crumb: v
                            .get_path("crumb")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    }),
                    _ => None,
                };
                Ok(ClientEffect::Navigate {
                    path,
                    params,
                    options,
                })
            }
            "navigate-back" => Ok(ClientEffect::NavigateBack),
            "host-ask" => {
                if !(3..=4).contains(&items.len()) {
                    return Err(DeError::custom(
                        "host-ask tuple takes an operator and its arguments: [\"host-ask\", op, [args]] or [\"host-ask\", op, [args], completion]",
                    ));
                }
                let op = items[1]
                    .as_str()
                    .ok_or_else(|| DeError::custom("host-ask tuple: op must be a string"))?
                    .to_string();
                let args = items[2]
                    .as_array()
                    .ok_or_else(|| DeError::custom("host-ask tuple: args must be an array"))?
                    .to_vec();
                let completion = items
                    .get(3)
                    .map(|v| HostAskCompletion::from_value(v).map_err(DeError::custom))
                    .transpose()?;
                Ok(ClientEffect::HostAsk { op, args, completion })
            }
            other => Err(DeError::custom(format!(
                "unknown client-effect tag: {other}"
            ))),
        }
    }
}

impl<'de> Deserialize<'de> for ClientEffect {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(ClientEffectVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::ValueMap;

    fn roundtrip(effect: &ClientEffect, expected_json: &str) {
        let json = serde_json::to_string(effect).unwrap();
        let actual: serde_json::Value = serde_json::from_str(&json).unwrap();
        let expected: serde_json::Value = serde_json::from_str(expected_json).unwrap();
        assert_eq!(actual, expected, "serialize mismatch for {effect:?}");

        let back: ClientEffect = serde_json::from_str(expected_json).unwrap();
        assert_eq!(&back, effect, "deserialize mismatch for {expected_json}");
    }

    #[test]
    fn render_ui_arity_3() {
        let effect = ClientEffect::RenderUi {
            slot: "main".to_string(),
            config: Some(Value::object(ValueMap::from_iter([(
                "type".into(),
                Value::String(("card".to_string()).into()),
            )]))),
        };
        roundtrip(&effect, r#"["render-ui","main",{"type":"card"}]"#);
    }

    #[test]
    fn render_ui_arity_3_null_config() {
        let effect = ClientEffect::RenderUi {
            slot: "main".to_string(),
            config: None,
        };
        roundtrip(&effect, r#"["render-ui","main",null]"#);
    }

    #[test]
    fn navigate_arity_2() {
        let effect = ClientEffect::Navigate {
            path: "/notes".to_string(),
            params: None,
            options: None,
        };
        roundtrip(&effect, r#"["navigate","/notes"]"#);
    }

    #[test]
    fn navigate_arity_3() {
        let effect = ClientEffect::Navigate {
            path: "/notes/:id".to_string(),
            params: Some(Value::object(ValueMap::from_iter([(
                "id".into(),
                Value::String(("42".to_string()).into()),
            )]))),
            options: None,
        };
        roundtrip(&effect, r#"["navigate","/notes/:id",{"id":"42"}]"#);
    }

    #[test]
    fn navigate_arity_3_null_params() {
        let effect = ClientEffect::Navigate {
            path: "/notes".to_string(),
            params: None,
            options: None,
        };
        // Arity-2 is what we EMIT for no params; but arity-3 with a null params slot
        // must still decode as the no-params case (the wire is broader than what we
        // choose to emit — TS may send `undefined` there, which JSON-stringifies null).
        let decoded: ClientEffect = serde_json::from_str(r#"["navigate","/notes",null]"#).unwrap();
        assert_eq!(decoded, effect);
    }

    #[test]
    fn navigate_arity_4() {
        let effect = ClientEffect::Navigate {
            path: "/notes/:id".to_string(),
            params: Some(Value::object(ValueMap::from_iter([(
                "id".into(),
                Value::String(("42".to_string()).into()),
            )]))),
            options: Some(NavigateOptions {
                crumb: Some("Note 42".to_string()),
            }),
        };
        roundtrip(
            &effect,
            r#"["navigate","/notes/:id",{"id":"42"},{"crumb":"Note 42"}]"#,
        );
    }

    #[test]
    fn navigate_arity_4_empty_options() {
        let effect = ClientEffect::Navigate {
            path: "/notes".to_string(),
            params: None,
            options: Some(NavigateOptions { crumb: None }),
        };
        roundtrip(&effect, r#"["navigate","/notes",null,{}]"#);
    }

    #[test]
    fn navigate_back() {
        roundtrip(&ClientEffect::NavigateBack, r#"["navigate-back"]"#);
    }

    #[test]
    fn from_effect_render_ui() {
        let effect = Effect::RenderUi {
            slot: "main".to_string(),
            content: Value::object(ValueMap::from_iter([(
                "type".into(),
                Value::String(("card".to_string()).into()),
            )])),
        };
        let client = ClientEffect::from_effect(&effect).unwrap();
        assert_eq!(
            client,
            ClientEffect::RenderUi {
                slot: "main".to_string(),
                config: Some(Value::object(ValueMap::from_iter([(
                    "type".into(),
                    Value::String(("card".to_string()).into())
                )]))),
            }
        );
    }

    #[test]
    fn from_effect_navigate() {
        let effect = Effect::Navigate {
            path: "/notes".to_string(),
            params: None,
            options: None,
        };
        let client = ClientEffect::from_effect(&effect).unwrap();
        assert_eq!(
            client,
            ClientEffect::Navigate {
                path: "/notes".to_string(),
                params: None,
                options: None
            }
        );
    }

    #[test]
    fn from_effect_navigate_carries_crumb_options() {
        let effect = Effect::Navigate {
            path: "/notes/:id".to_string(),
            params: None,
            options: Some(NavigateOptions {
                crumb: Some("My Note".to_string()),
            }),
        };
        let client = ClientEffect::from_effect(&effect).unwrap();
        assert_eq!(
            client,
            ClientEffect::Navigate {
                path: "/notes/:id".to_string(),
                params: None,
                options: Some(NavigateOptions {
                    crumb: Some("My Note".to_string())
                }),
            }
        );
    }

    #[test]
    fn from_effect_navigate_back() {
        assert_eq!(
            ClientEffect::from_effect(&Effect::NavigateBack),
            Some(ClientEffect::NavigateBack)
        );
    }

    #[test]
    fn host_ask_arity_3_carries_the_op_and_its_arguments() {
        let effect = ClientEffect::HostAsk {
            op: "audio/play-sound".to_string(),
            args: vec![Value::String("footstep".to_string().into())],
            completion: None,
        };
        roundtrip(&effect, r#"["host-ask","audio/play-sound",["footstep"]]"#);
        let bare = ClientEffect::HostAsk { op: "auth/sign-out".to_string(), args: vec![], completion: None };
        roundtrip(&bare, r#"["host-ask","auth/sign-out",[]]"#);
    }

    #[test]
    fn host_ask_arity_4_carries_the_return_address() {
        let routed = ClientEffect::HostAsk {
            op: "auth/sign-in".to_string(),
            args: vec![],
            completion: Some(HostAskCompletion {
                trait_name: "SignIn".to_string(),
                entity: Some("row-1".to_string()),
                key: Some("k".to_string()),
                success: Some("SIGNED_IN".to_string()),
                failure: Some("SIGN_IN_FAILED".to_string()),
            }),
        };
        roundtrip(
            &routed,
            r#"["host-ask","auth/sign-in",[],{"trait":"SignIn","entity":"row-1","key":"k","success":"SIGNED_IN","failure":"SIGN_IN_FAILED"}]"#,
        );
        let success_only = ClientEffect::HostAsk {
            op: "auth/sign-in".to_string(),
            args: vec![],
            completion: Some(HostAskCompletion { trait_name: "SignIn".to_string(), success: Some("OK".to_string()), ..Default::default() }),
        };
        roundtrip(&success_only, r#"["host-ask","auth/sign-in",[],{"trait":"SignIn","success":"OK"}]"#);
    }

    #[test]
    fn a_completion_is_declared_only_by_routes_and_names_its_trait() {
        let routed = |key: Option<&str>, success: Option<&str>, failure: Option<&str>| Effect::Custom {
            name: "auth/sign-in".to_string(),
            args: vec![],
            key: key.map(|k| Value::String(k.to_string().into())),
            emit: Some(crate::effect_types::FetchEmit { success: success.map(str::to_string), failure: failure.map(str::to_string), ..Default::default() }),
        };
        let asked = EffectResult::host_asked(routed(Some("k"), Some("OK"), Some("BAD")));
        assert_eq!(
            ClientEffect::from_result_in(&asked, Some(("SignIn", Some("row-1")))),
            Some(ClientEffect::HostAsk {
                op: "auth/sign-in".to_string(),
                args: vec![],
                completion: Some(HostAskCompletion {
                    trait_name: "SignIn".to_string(),
                    entity: Some("row-1".to_string()),
                    key: Some("k".to_string()),
                    success: Some("OK".to_string()),
                    failure: Some("BAD".to_string()),
                }),
            })
        );
        let undeclared = EffectResult::host_asked(routed(None, None, None));
        assert_eq!(
            ClientEffect::from_result_in(&undeclared, Some(("SignIn", None))),
            Some(ClientEffect::HostAsk { op: "auth/sign-in".to_string(), args: vec![], completion: None }),
            "control: an ask with no route has no return address"
        );
    }

    #[test]
    fn a_malformed_host_ask_tuple_is_refused() {
        for (wire, reason) in [
            (r#"["host-ask","auth/sign-out"]"#, "takes an operator and its arguments"),
            (r#"["host-ask","auth/sign-out",[],{"trait":"T"},1]"#, "takes an operator and its arguments"),
            (r#"["host-ask","auth/sign-in",[],1]"#, "completion"),
            (r#"["host-ask","auth/sign-in",[],{"success":"OK"}]"#, "completion needs the asking trait"),
            (r#"["host-ask","auth/sign-in",[],{"trait":"T","success":3}]"#, "completion success must be a string"),
            (r#"["host-ask",3,[]]"#, "op must be a string"),
            (r#"["host-ask","audio/play-sound","footstep"]"#, "args must be an array"),
        ] {
            let err = serde_json::from_str::<ClientEffect>(wire).unwrap_err().to_string();
            assert!(err.contains(reason), "{wire}: {err}");
        }
    }

    #[test]
    fn only_a_host_asked_result_is_a_host_ask() {
        let custom = Effect::Custom {
            name: "auth/sign-out".to_string(),
            args: vec![],
            key: None,
            emit: None,
        };
        assert_eq!(
            ClientEffect::from_result(&EffectResult::host_asked(custom.clone())),
            Some(ClientEffect::HostAsk { op: "auth/sign-out".to_string(), args: vec![], completion: None })
        );
        assert_eq!(ClientEffect::from_result(&EffectResult::executed(custom.clone())), None, "control: a server-run custom effect reaches no client");
        assert_eq!(ClientEffect::from_result(&EffectResult::noop(custom, "x")), None);
        assert_eq!(
            ClientEffect::from_result(&EffectResult::executed(Effect::NavigateBack)),
            Some(ClientEffect::NavigateBack),
            "control: the tuple effects still convert from their result"
        );
    }

    #[test]
    fn a_render_ui_tuple_longer_than_slot_and_pattern_is_refused() {
        for wire in [
            r#"["render-ui","detail",{"type":"detail-panel"},{"title":"Note"}]"#,
            r#"["render-ui","toast",{"type":"toast"},null,3]"#,
        ] {
            let err = serde_json::from_str::<ClientEffect>(wire)
                .unwrap_err()
                .to_string();
            assert!(err.contains("takes a slot and a pattern"), "{wire}: {err}");
        }
    }

    #[test]
    fn render_ui_value_is_the_pattern_and_an_empty_object_for_none() {
        let effect = ClientEffect::RenderUi {
            slot: "modal".to_string(),
            config: Some(Value::object(ValueMap::from_iter([(
                "type".into(),
                Value::String(("form".to_string()).into()),
            )]))),
        };
        assert_eq!(
            effect
                .render_ui_value()
                .unwrap()
                .get_path("type")
                .and_then(Value::as_str),
            Some("form")
        );
        let empty = ClientEffect::RenderUi {
            slot: "main".to_string(),
            config: None,
        };
        assert_eq!(
            empty.render_ui_value(),
            Some(Value::Object(Default::default()))
        );
    }

    #[test]
    fn render_ui_value_none_for_non_render_ui() {
        assert_eq!(ClientEffect::NavigateBack.render_ui_value(), None);
    }

    #[test]
    fn from_effect_non_client_effect_is_none() {
        let effect = Effect::Emit {
            event: "X".to_string(),
            payload: Value::Null,
        };
        assert_eq!(ClientEffect::from_effect(&effect), None);
    }
}
