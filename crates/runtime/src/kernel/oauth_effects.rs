//! The OAuth asks: authorize, the pending grant, and connections.

use super::*;

impl RuntimeKernel {
    pub(super) fn oauth_failed(effect: &Effect, reason: OAuthFailureReason) -> EffectResult {
        let payload = Value::object(ValueMap::from_iter([
            (
                "error".into(),
                Value::String(reason.message().to_string().into()),
            ),
            ("code".into(), Value::String(reason.code().to_string().into())),
            (
                "reason".into(),
                Value::String(reason.as_str().to_string().into()),
            ),
        ]));
        http_failure(effect, payload)
    }

    /// `oauth-authorize`: build the authorization URL and keep the pending grant (PKCE verifier, subject) with the
    /// host. The state and verifier come from two journaled `Random` asks; the verifier never reaches the payload.
    pub(super) fn run_oauth_authorize(
        &self,
        effect: &Effect,
        client: &Value,
        options: &Value,
        principal: Option<&str>,
    ) -> EffectResult {
        let key = match effect {
            Effect::OAuthAuthorize { key: Some(key), .. } => {
                match orbital_core_base::effect_types::call_key_text(key) {
                    Ok(text) => Value::string(text.to_string()),
                    Err(error) => return EffectResult::failed(effect.clone(), &error.to_string()),
                }
            }
            _ => Value::Null,
        };
        let mut result = self.authorize_inner(effect, client, options, principal);
        let payload = match result.return_value.take() {
            Some(Value::Object(mut fields)) => {
                fields.insert("key".into(), key);
                Value::Object(fields)
            }
            _ => Value::object(ValueMap::from_iter([
                (
                    "error".into(),
                    Value::String(result.error.clone().unwrap_or_default().into()),
                ),
                ("key".into(), key),
            ])),
        };
        result.return_value = Some(payload);
        result
    }

    pub(super) fn authorize_inner(
        &self,
        effect: &Effect,
        client: &Value,
        options: &Value,
        principal: Option<&str>,
    ) -> EffectResult {
        use crate::oauth_flow::{
            build_authorization, build_mock_authorization, declared_metadata, plan_authorize,
            FlowFailure, MOCK_PENDING_TTL_MS,
        };
        use orbital_core_base::oauth_ask::OAuthAsk;
        use orbital_core_base::oauth_client::OAuthMode;
        use orbital_core_base::oauth_client::OAuthServerDecl;
        let plan = match plan_authorize(client, options) {
            Ok(plan) => plan,
            Err(message) => return EffectResult::failed(effect.clone(), &message),
        };
        let decl = &plan.client;
        if decl.mode == OAuthMode::Mock {
            let state_bytes = match self.io.random(16) {
                Ok(bytes) => bytes,
                Err(message) => return EffectResult::failed(effect.clone(), &message),
            };
            let (auth_url, state, pending) = build_mock_authorization(&plan, &state_bytes);
            return self.keep_pending_grant(
                effect,
                auth_url,
                state,
                pending,
                MOCK_PENDING_TTL_MS,
                principal,
                false,
            );
        }
        let who = match decl.client_id.scope {
            orbital_core_base::http_wire::SecretScope::App => None,
            orbital_core_base::http_wire::SecretScope::User => match principal {
                Some(principal) => Some(principal),
                None => return Self::oauth_failed(effect, OAuthFailureReason::NotSignedIn),
            },
        };
        let client_id = match self.io.secret(&decl.client_id.name, who) {
            Ok(Some(id)) => id,
            Ok(None) => return Self::oauth_failed(effect, OAuthFailureReason::UnconfiguredClient),
            Err(message) => return EffectResult::failed(effect.clone(), &message),
        };
        if let Some(slot) = &decl.client_secret {
            let secret_who = match slot.scope {
                orbital_core_base::http_wire::SecretScope::App => None,
                orbital_core_base::http_wire::SecretScope::User => match principal {
                    Some(principal) => Some(principal),
                    None => return Self::oauth_failed(effect, OAuthFailureReason::NotSignedIn),
                },
            };
            match self.io.secret(&slot.name, secret_who) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Self::oauth_failed(effect, OAuthFailureReason::UnconfiguredClient)
                }
                Err(message) => return EffectResult::failed(effect.clone(), &message),
            }
        }
        let meta = match &decl.server {
            OAuthServerDecl::Endpoints { .. } => declared_metadata(&decl.server),
            OAuthServerDecl::Issuer(issuer) => {
                match self.io.oauth(
                    &OAuthAsk::MetadataGet {
                        issuer: issuer.clone(),
                    },
                    principal,
                ) {
                    Ok(OAuthReply::Metadata { metadata }) => metadata,
                    Ok(_) | Err(_) => {
                        return Self::oauth_failed(effect, OAuthFailureReason::StoreUnavailable)
                    }
                }
            }
        };
        let Some(meta) = meta else {
            return Self::oauth_failed(effect, OAuthFailureReason::MetadataInvalid);
        };
        let (state_bytes, verifier_bytes) = match (self.io.random(32), self.io.random(32)) {
            (Ok(state), Ok(verifier)) => (state, verifier),
            (Err(message), _) | (_, Err(message)) => {
                return EffectResult::failed(effect.clone(), &message)
            }
        };
        let (auth_url, state, pending) =
            match build_authorization(&plan, &client_id, &meta, state_bytes, verifier_bytes) {
                Ok(built) => built,
                Err(FlowFailure::Reason(reason)) => return Self::oauth_failed(effect, reason),
                Err(FlowFailure::Refused(refusal)) => {
                    return EffectResult::failed(effect.clone(), &refusal.message())
                }
                Err(FlowFailure::Transport(_)) => {
                    return Self::oauth_failed(effect, OAuthFailureReason::ResponseInvalid)
                }
            };
        self.keep_pending_grant(
            effect,
            auth_url,
            state,
            pending,
            10 * 60 * 1000,
            principal,
            true,
        )
    }

    /// Hand the pending grant to the host and report the authorization URL and state. A real (not mock) authorization then
    /// asks the host for the person's consent: a host that can open the URL answers the code, any other answers none.
    pub(super) fn keep_pending_grant(
        &self,
        effect: &Effect,
        auth_url: String,
        state: String,
        pending: orbital_core_base::oauth_wire::Pending,
        ttl_ms: u64,
        principal: Option<&str>,
        consent: bool,
    ) -> EffectResult {
        use orbital_core_base::oauth_ask::OAuthAsk;
        let redirect_uri = pending.redirect_uri.clone();
        let put = OAuthAsk::PendingPut {
            state: state.clone(),
            grant: pending,
            ttl_ms,
        };
        match self.io.oauth(&put, principal) {
            Ok(OAuthReply::Done) => {}
            Ok(_) | Err(_) => {
                return Self::oauth_failed(effect, OAuthFailureReason::StoreUnavailable)
            }
        }
        let code = if consent {
            let ask = OAuthAsk::Consent {
                auth_url: auth_url.clone(),
                state: state.clone(),
                redirect_uri,
            };
            match self.io.oauth(&ask, principal) {
                Ok(OAuthReply::Code { code }) => code.unwrap_or_default(),
                Ok(_) | Err(_) => {
                    return Self::oauth_failed(effect, OAuthFailureReason::StoreUnavailable)
                }
            }
        } else {
            String::new()
        };
        EffectResult::executed_with_value(
            effect.clone(),
            Value::object(ValueMap::from_iter([
                ("authUrl".into(), Value::String(auth_url.into())),
                ("state".into(), Value::String(state.into())),
                ("code".into(), Value::String(code.into())),
            ])),
        )
    }

    /// `oauth-connection-status` / `oauth-connection-forget`: store first, then caller (the legacy order).
    pub(super) fn run_oauth_connection(
        &self,
        effect: &Effect,
        provider: &Value,
        principal: Option<&str>,
    ) -> EffectResult {
        use orbital_core_base::oauth_ask::{OAuthAsk, OAuthProvider};
        let status = matches!(effect, Effect::OAuthConnectionStatus { .. });
        let head = if status {
            "oauth-connection-status"
        } else {
            "oauth-connection-forget"
        };
        let provider = match provider {
            Value::String(word) => match OAuthProvider::parse(word) {
                Some(provider) => provider,
                None => {
                    return EffectResult::failed(
                        effect.clone(),
                        &format!("{head}: provider must be google, github or auth0, not `{word}`"),
                    )
                }
            },
            _ => {
                return EffectResult::failed(
                    effect.clone(),
                    &format!("{head}: provider must be a provider word"),
                )
            }
        };
        match self.io.oauth(&OAuthAsk::ConnectionAvailable, principal) {
            Ok(OAuthReply::Flag { value: true }) => {}
            Ok(_) | Err(_) => {
                return Self::oauth_failed(effect, OAuthFailureReason::StoreUnavailable)
            }
        }
        let Some(caller) = principal.filter(|p| !p.is_empty()) else {
            return Self::oauth_failed(effect, OAuthFailureReason::NotSignedIn);
        };
        let ask = if status {
            OAuthAsk::ConnectionStatus { provider }
        } else {
            OAuthAsk::ConnectionRemove { provider }
        };
        match self.io.oauth(&ask, Some(caller)) {
            Ok(OAuthReply::Row { row }) => EffectResult::executed_with_value(
                effect.clone(),
                crate::oauth_flow::row_value(&row),
            ),
            Ok(_) | Err(_) => Self::oauth_failed(effect, OAuthFailureReason::StoreUnavailable),
        }
    }
}
