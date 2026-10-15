//! Client-effect wire grammar — the Rust mirror of `@almadar/core`'s
//! `ClientEffectTuple` (`packages/almadar-core/src/types/bus.ts`).
//!
//! The client only ever receives `render-ui` / `navigate` / `navigate-back` on this
//! channel (server-only effects report through `ServerEffectResult`, and `emit`
//! surfaces as `EmittedEvent` — see `effects::response`). The wire shape is a JSON
//! ARRAY whose arity carries which trailing fields are present, so this is the one
//! type in the crate with a hand-written `Serialize`/`Deserialize` instead of a
//! derive — no other shape in the wire needs positional (rather than keyed) framing.

use crate::evaluator::Value;
use serde::de::{Error as DeError, SeqAccess, Visitor};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

use super::Effect;

/// `{ crumb?: string }` — the optional fourth element of a `navigate` tuple.
#[cfg_attr(test, derive(ts_rs::TS), ts(optional_fields))]
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
    RenderUi {
        slot: String,
        /// `PatternConfig | null` — always present on the wire, `null` when absent.
        config: Option<Value>,
        /// `ResolvedPatternProps` — present only in the arity-4/5 forms.
        props: Option<Value>,
        /// Render priority — present only in the arity-5 form.
        priority: Option<u32>,
    },
    Navigate {
        path: String,
        params: Option<Value>,
        options: Option<NavigateOptions>,
    },
    NavigateBack,
}

impl ClientEffect {
    /// Convert a resolved internal [`Effect`] to its wire tuple, if the effect is
    /// client-targeted in tuple form (`render-ui`/`navigate`/`navigate-back`; `emit`
    /// surfaces separately as an [`super::EmittedEvent`], never here).
    ///
    /// `RenderUi`'s whole (already-bound) content becomes `config` — the Rust kernel
    /// resolves `@entity`/`@payload` bindings once, server-side, into one merged
    /// object, unlike the TS runtime's separate config/props layering, so `props`
    /// stays `None` on this path.
    /// For `RenderUi`, the pattern object a renderer parses: `config` (the `type` +
    /// declared config) with `props`' keys layered on top (the resolved/interpolated
    /// values win). Native (non-web) clients that predate the config/props split read
    /// ONE merged pattern object, same as the legacy `Effect::RenderUi { content }`
    /// shape did — this is that convenience, kept in one place instead of re-merged at
    /// every call site.
    pub fn render_ui_value(&self) -> Option<Value> {
        let ClientEffect::RenderUi { config, props, .. } = self else {
            return None;
        };
        let mut merged = config
            .clone()
            .unwrap_or_else(|| Value::Object(Default::default()));
        if let (Some(extra), Some(base)) = (
            props.as_ref().and_then(Value::as_object),
            merged.as_object_mut(),
        ) {
            for (k, v) in extra {
                base.insert(k.clone(), v.clone());
            }
        }
        Some(merged)
    }

    pub fn from_effect(effect: &Effect) -> Option<ClientEffect> {
        match effect {
            Effect::RenderUi { slot, content } => Some(ClientEffect::RenderUi {
                slot: slot.clone(),
                config: if content.is_null() {
                    None
                } else {
                    Some(content.clone())
                },
                props: None,
                priority: None,
            }),
            Effect::Navigate {
                path,
                params,
                options,
            } => Some(ClientEffect::Navigate {
                path: path.clone(),
                params: params.as_ref().map(|p| Value::Object(p.clone())),
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
            ClientEffect::RenderUi {
                slot,
                config,
                props,
                priority,
            } => {
                let len = if priority.is_some() {
                    5
                } else if props.is_some() {
                    4
                } else {
                    3
                };
                let mut seq = serializer.serialize_seq(Some(len))?;
                seq.serialize_element("render-ui")?;
                seq.serialize_element(slot)?;
                serialize_opt_value(&mut seq, config.as_ref())?;
                if len >= 4 {
                    serialize_opt_value(&mut seq, props.as_ref())?;
                }
                if len == 5 {
                    match priority {
                        Some(p) => seq.serialize_element(p)?,
                        None => seq.serialize_element(&Value::Null)?,
                    }
                }
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
        }
    }
}

struct ClientEffectVisitor;

impl<'de> Visitor<'de> for ClientEffectVisitor {
    type Value = ClientEffect;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a client-effect tuple: [\"render-ui\"|\"navigate\"|\"navigate-back\", ...]")
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
                if items.len() < 3 {
                    return Err(DeError::custom(
                        "render-ui tuple needs at least [\"render-ui\", slot, config]",
                    ));
                }
                let slot = items[1]
                    .as_str()
                    .ok_or_else(|| DeError::custom("render-ui tuple: slot must be a string"))?
                    .to_string();
                let config = opt(items.get(2));
                let props = opt(items.get(3));
                let priority = items
                    .get(4)
                    .and_then(|v| if v.is_null() { None } else { v.as_number() })
                    .map(|n| n as u32);
                Ok(ClientEffect::RenderUi {
                    slot,
                    config,
                    props,
                    priority,
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
    use crate::evaluator::ValueMap;

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
            config: Some(Value::Object(ValueMap::from([(
                "type".to_string(),
                Value::String("card".to_string()),
            )]))),
            props: None,
            priority: None,
        };
        roundtrip(&effect, r#"["render-ui","main",{"type":"card"}]"#);
    }

    #[test]
    fn render_ui_arity_3_null_config() {
        let effect = ClientEffect::RenderUi {
            slot: "main".to_string(),
            config: None,
            props: None,
            priority: None,
        };
        roundtrip(&effect, r#"["render-ui","main",null]"#);
    }

    #[test]
    fn render_ui_arity_4() {
        let effect = ClientEffect::RenderUi {
            slot: "detail".to_string(),
            config: Some(Value::Object(ValueMap::from([(
                "type".to_string(),
                Value::String("detail-panel".to_string()),
            )]))),
            props: Some(Value::Object(ValueMap::from([(
                "title".to_string(),
                Value::String("Note".to_string()),
            )]))),
            priority: None,
        };
        roundtrip(
            &effect,
            r#"["render-ui","detail",{"type":"detail-panel"},{"title":"Note"}]"#,
        );
    }

    #[test]
    fn render_ui_arity_5() {
        let effect = ClientEffect::RenderUi {
            slot: "toast".to_string(),
            config: Some(Value::Object(ValueMap::from([(
                "type".to_string(),
                Value::String("toast".to_string()),
            )]))),
            props: None,
            priority: Some(3),
        };
        roundtrip(&effect, r#"["render-ui","toast",{"type":"toast"},null,3]"#);
    }

    #[test]
    fn render_ui_arity_5_with_props() {
        let effect = ClientEffect::RenderUi {
            slot: "toast".to_string(),
            config: Some(Value::Object(ValueMap::new())),
            props: Some(Value::Object(ValueMap::from([(
                "message".to_string(),
                Value::String("Saved".to_string()),
            )]))),
            priority: Some(1),
        };
        roundtrip(&effect, r#"["render-ui","toast",{},{"message":"Saved"},1]"#);
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
            params: Some(Value::Object(ValueMap::from([(
                "id".to_string(),
                Value::String("42".to_string()),
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
            params: Some(Value::Object(ValueMap::from([(
                "id".to_string(),
                Value::String("42".to_string()),
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
            content: Value::Object(ValueMap::from([(
                "type".to_string(),
                Value::String("card".to_string()),
            )])),
        };
        let client = ClientEffect::from_effect(&effect).unwrap();
        assert_eq!(
            client,
            ClientEffect::RenderUi {
                slot: "main".to_string(),
                config: Some(Value::Object(ValueMap::from([(
                    "type".to_string(),
                    Value::String("card".to_string())
                )]))),
                props: None,
                priority: None,
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
    fn render_ui_value_merges_props_over_config() {
        let effect = ClientEffect::RenderUi {
            slot: "modal".to_string(),
            config: Some(Value::Object(ValueMap::from([
                ("type".to_string(), Value::String("form".to_string())),
                ("title".to_string(), Value::String("stale".to_string())),
            ]))),
            props: Some(Value::Object(ValueMap::from([(
                "title".to_string(),
                Value::String("fresh".to_string()),
            )]))),
            priority: None,
        };
        let merged = effect.render_ui_value().unwrap();
        assert_eq!(
            merged.get_path("type").and_then(Value::as_str),
            Some("form")
        );
        assert_eq!(
            merged.get_path("title").and_then(Value::as_str),
            Some("fresh")
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
