//! The `oauth` row kind: a granted access token (and its refresh token) the store holds for one (name, principal), and the
//! exchange that creates it. The program that asked never sees a token; it gets `{connected, label}`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::sync::Arc;

use orbital_core_base::sync::Mutex;
use serde::{Deserialize, Serialize};

use super::{storage_key, EncryptedCredentialStore, RowBody, OAUTH_KIND};
use crate::credential_admin::{
    ConnectFailure, ConnectFailureKind, CredentialGrant, CredentialStanding,
};
use orbital_core_base::credential_grant::{
    assertion_jwt, authorization_code_request, json_token_refresh_request, json_token_request,
    jwt_bearer_request, parse_json_token_response, parse_token_response, refresh_token_request,
    AssertionSpec, ClientAuth, CodeExchange, GrantError, GrantErrorKind, JsonTokenSpec,
    RefreshExchange,
};

const OAUTH_SERVICE: &str = "oauth";
const RECORD_VERSION: u8 = 1;

/// The transport could not complete the POST (no route, refused egress, TLS, timeout).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError(pub String);

/// The host's one network capability the store needs: a form POST to an https token endpoint, answering the status and
/// the body. No redirects are followed.
pub trait CredentialTransport: Send + Sync {
    fn post_form(
        &self,
        endpoint: &str,
        headers: &[(String, String)],
        body: &str,
    ) -> Result<(u16, String), TransportError>;
}

/// What a host sends for one token exchange, and the ticket that [`EncryptedCredentialStore::connect_finish`] needs to
/// persist its outcome. The ticket never leaves the process; a wasm host holds it behind an opaque handle.
pub struct PendingExchange {
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub ticket: ExchangeTicket,
}

impl std::fmt::Debug for PendingExchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingExchange").field("endpoint", &self.endpoint).finish_non_exhaustive()
    }
}

/// What the store needs to finish one exchange: the row's identity, where it was granted, which client credentials it
/// names, and (for a refresh) the refresh token being rotated.
pub struct ExchangeTicket {
    exchange: Exchange,
}

enum Exchange {
    Code {
        name: String,
        principal: Option<String>,
        token_endpoint: String,
        client_id_ref: String,
        client_secret_ref: Option<String>,
        client_auth: ClientAuth,
    },
    Refresh {
        name: String,
        principal: Option<String>,
        token_endpoint: String,
        refresh: String,
        client_id_ref: String,
        client_secret_ref: Option<String>,
        client_auth: ClientAuth,
        label: Option<String>,
    },
    Assertion {
        name: String,
        principal: Option<String>,
        token_endpoint: String,
        spec: AssertionSpec,
        scopes: Vec<String>,
    },
    JsonMint {
        name: String,
        principal: Option<String>,
        token_endpoint: String,
        spec: JsonTokenSpec,
        client_id_ref: String,
        client_secret_ref: String,
    },
    JsonRefresh {
        name: String,
        principal: Option<String>,
        token_endpoint: String,
        spec: JsonTokenSpec,
        refresh: String,
        client_id_ref: String,
        client_secret_ref: String,
    },
}

impl Drop for ExchangeTicket {
    fn drop(&mut self) {
        if let Exchange::Refresh { refresh, .. } | Exchange::JsonRefresh { refresh, .. } = &mut self.exchange {
            zeroize::Zeroize::zeroize(refresh);
        }
    }
}

impl std::fmt::Debug for ExchangeTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExchangeTicket").finish_non_exhaustive()
    }
}

/// How a resolve of a possibly-expired credential continues: with its answer, or with a refresh the host must send.
#[derive(Debug)]
pub enum RefreshStart {
    Resolved(Option<String>),
    Exchange(PendingExchange),
}

/// The sealed content of an `oauth` row.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct OauthRecord {
    v: u8,
    pub(super) access: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<i64>,
    token_endpoint: String,
    #[serde(default)]
    client_id_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_secret_ref: Option<String>,
    client_auth: ClientAuth,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    assertion: Option<AssertionSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    json_token: Option<JsonTokenSpec>,
}

impl OauthRecord {
    pub(super) fn parse(plaintext: &str) -> Option<Self> {
        serde_json::from_str::<Self>(plaintext)
            .ok()
            .filter(|record| record.v == RECORD_VERSION && !record.access.is_empty())
    }

    pub(super) fn zeroize(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.access);
        if let Some(refresh) = &mut self.refresh {
            zeroize::Zeroize::zeroize(refresh);
        }
    }
}

impl std::fmt::Debug for OauthRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OauthRecord")
            .field("tokenEndpoint", &self.token_endpoint)
            .finish_non_exhaustive()
    }
}

fn failure(kind: ConnectFailureKind, message: impl Into<String>) -> ConnectFailure {
    ConnectFailure::new(kind, message)
}

fn from_grant_error(error: GrantError) -> ConnectFailure {
    let kind = match error.kind {
        GrantErrorKind::InvalidGrant => ConnectFailureKind::InvalidGrant,
        GrantErrorKind::InvalidClient => ConnectFailureKind::InvalidClient,
        GrantErrorKind::Refused => ConnectFailureKind::Refused,
        GrantErrorKind::Malformed => ConnectFailureKind::Malformed,
    };
    failure(kind, error.message)
}

impl EncryptedCredentialStore {
    /// Exchange `grant.code` at the grant's token endpoint through `transport` and keep the result as an `oauth` row of
    /// `grant.name` for `grant.principal`. Client credentials are read from this store's own app-wide rows by the names the
    /// grant carries. A failure persists nothing.
    pub fn connect(
        &self,
        grant: &CredentialGrant,
        transport: &dyn CredentialTransport,
    ) -> Result<CredentialStanding, ConnectFailure> {
        let flight = self.flight(&grant.name, grant.principal.as_deref());
        let _turn = (grant.grant == "refresh_token").then(|| flight.lock());
        let pending = self.connect_begin(grant)?;
        self.send_exchange(pending, transport)
    }

    /// The request a host sends for `grant`, and the ticket to finish it with. Touches no network and persists nothing.
    pub fn connect_begin(&self, grant: &CredentialGrant) -> Result<PendingExchange, ConnectFailure> {
        if !matches!(
            grant.grant.as_str(),
            "authorization_code" | "refresh_token" | "jwt-bearer" | "json-token"
        ) {
            return Err(failure(
                ConnectFailureKind::Refused,
                format!(
                    "credential-connect: grant `{}` is not supported",
                    grant.grant
                ),
            ));
        }
        if grant.principal.as_deref().is_some_and(str::is_empty) {
            return Err(failure(
                ConnectFailureKind::Unsigned,
                "credential-connect: the principal must be a nonempty string",
            ));
        }
        if !self.enabled() {
            return Err(failure(
                ConnectFailureKind::Unavailable,
                crate::credential_admin::NO_CONNECT,
            ));
        }
        self.ensure_warm()
            .map_err(|error| failure(ConnectFailureKind::Unavailable, error))?;
        match grant.grant.as_str() {
            "refresh_token" => self.refresh_grant_begin(grant),
            "jwt-bearer" => {
                let spec = grant.assertion.as_ref().ok_or_else(|| {
                    failure(
                        ConnectFailureKind::Refused,
                        "credential-connect: grant `jwt-bearer` carries no assertion",
                    )
                })?;
                self.assertion_pending(
                    &grant.name,
                    grant.principal.as_deref(),
                    &grant.token_endpoint,
                    spec,
                    &grant.scopes,
                )
            }
            "json-token" => {
                let spec = grant.json_token.as_ref().ok_or_else(|| {
                    failure(
                        ConnectFailureKind::Refused,
                        "credential-connect: grant `json-token` carries no token form",
                    )
                })?;
                let secret_ref = grant.client_secret.as_deref().ok_or_else(|| {
                    failure(
                        ConnectFailureKind::Refused,
                        "credential-connect: grant `json-token` needs clientSecret",
                    )
                })?;
                self.json_mint_pending(
                    &grant.name,
                    grant.principal.as_deref(),
                    &grant.token_endpoint,
                    spec,
                    &grant.client_id,
                    secret_ref,
                )
            }
            _ => self.code_begin(grant),
        }
    }

    fn code_begin(&self, grant: &CredentialGrant) -> Result<PendingExchange, ConnectFailure> {
        let (client_id, client_secret) =
            self.resolve_client(&grant.client_id, grant.client_secret.as_deref(), grant.client_auth)?;
        let request = authorization_code_request(&CodeExchange {
            code: grant.code.expose(),
            redirect_uri: &grant.redirect_uri,
            verifier: grant
                .verifier
                .as_ref()
                .map(|verifier| verifier.expose().as_str()),
            client_id: &client_id,
            client_secret: client_secret.as_deref(),
            client_auth: grant.client_auth,
        })
        .map_err(from_grant_error)?;
        Ok(PendingExchange {
            endpoint: grant.token_endpoint.clone(),
            headers: request.headers,
            body: request.body,
            ticket: ExchangeTicket {
                exchange: Exchange::Code {
                    name: grant.name.clone(),
                    principal: grant.principal.clone(),
                    token_endpoint: grant.token_endpoint.clone(),
                    client_id_ref: grant.client_id.clone(),
                    client_secret_ref: grant.client_secret.clone(),
                    client_auth: grant.client_auth,
                },
            },
        })
    }

    fn blinding_seed(&self) -> [u8; 32] {
        let mut seed = [0u8; 32];
        for chunk in seed.chunks_mut(12) {
            let iv = (self.iv)();
            let take = chunk.len();
            chunk.copy_from_slice(&iv[..take]);
        }
        seed
    }

    fn held_text(&self, name: &str, what: &str) -> Result<String, ConnectFailure> {
        self.resolve(name, None).filter(|value| !value.is_empty()).ok_or_else(|| {
            failure(
                ConnectFailureKind::Refused,
                format!("credential `{name}` ({what}) is not stored"),
            )
        })
    }

    fn assertion_pending(
        &self,
        name: &str,
        principal: Option<&str>,
        token_endpoint: &str,
        spec: &AssertionSpec,
        scopes: &[String],
    ) -> Result<PendingExchange, ConnectFailure> {
        let mut pem = self.held_text(&spec.signing_key, "the signing key")?;
        let signed = assertion_jwt(spec, scopes, &pem, (self.clock)(), self.blinding_seed());
        zeroize::Zeroize::zeroize(&mut pem);
        let assertion = signed.map_err(from_grant_error)?;
        let request = jwt_bearer_request(assertion.expose());
        Ok(PendingExchange {
            endpoint: token_endpoint.to_string(),
            headers: request.headers,
            body: request.body,
            ticket: ExchangeTicket {
                exchange: Exchange::Assertion {
                    name: name.to_string(),
                    principal: principal.map(str::to_string),
                    token_endpoint: token_endpoint.to_string(),
                    spec: spec.clone(),
                    scopes: scopes.to_vec(),
                },
            },
        })
    }

    fn json_mint_pending(
        &self,
        name: &str,
        principal: Option<&str>,
        token_endpoint: &str,
        spec: &JsonTokenSpec,
        client_id_ref: &str,
        client_secret_ref: &str,
    ) -> Result<PendingExchange, ConnectFailure> {
        let client_id = self.held_text(client_id_ref, "the client id")?;
        let client_secret = self.held_text(client_secret_ref, "the client secret")?;
        let request = json_token_request(spec, &client_id, &client_secret);
        Ok(PendingExchange {
            endpoint: token_endpoint.to_string(),
            headers: request.headers,
            body: request.body,
            ticket: ExchangeTicket {
                exchange: Exchange::JsonMint {
                    name: name.to_string(),
                    principal: principal.map(str::to_string),
                    token_endpoint: token_endpoint.to_string(),
                    spec: spec.clone(),
                    client_id_ref: client_id_ref.to_string(),
                    client_secret_ref: client_secret_ref.to_string(),
                },
            },
        })
    }

    fn json_refresh_pending(
        &self,
        name: &str,
        principal: Option<&str>,
        mint_endpoint: &str,
        spec: &JsonTokenSpec,
        refresh: String,
        refs: (&str, &str),
    ) -> Result<PendingExchange, ConnectFailure> {
        let (Some(endpoint), Some(request)) = (
            spec.refresh_endpoint.as_ref(),
            json_token_refresh_request(spec, &refresh),
        ) else {
            return Err(failure(
                ConnectFailureKind::Refused,
                "credential-connect: the token form declares no refresh endpoint",
            ));
        };
        Ok(PendingExchange {
            endpoint: endpoint.clone(),
            headers: request.headers,
            body: request.body,
            ticket: ExchangeTicket {
                exchange: Exchange::JsonRefresh {
                    name: name.to_string(),
                    principal: principal.map(str::to_string),
                    token_endpoint: mint_endpoint.to_string(),
                    spec: spec.clone(),
                    refresh,
                    client_id_ref: refs.0.to_string(),
                    client_secret_ref: refs.1.to_string(),
                },
            },
        })
    }

    fn send_exchange(
        &self,
        pending: PendingExchange,
        transport: &dyn CredentialTransport,
    ) -> Result<CredentialStanding, ConnectFailure> {
        let (status, body) = transport
            .post_form(&pending.endpoint, &pending.headers, &pending.body)
            .map_err(|error| failure(ConnectFailureKind::Transport, error.0))?;
        self.connect_finish(pending.ticket, status, &body)
    }

    /// Settle an exchange with the token endpoint's `status` and `body`: parse the tokens and keep them as the row, or
    /// fail (an `invalid_grant` on a refresh drops the row). The ticket is consumed.
    pub fn connect_finish(
        &self,
        ticket: ExchangeTicket,
        status: u16,
        body: &str,
    ) -> Result<CredentialStanding, ConnectFailure> {
        match &ticket.exchange {
            Exchange::Code {
                name,
                principal,
                token_endpoint,
                client_id_ref,
                client_secret_ref,
                client_auth,
            } => {
                let token = parse_token_response(status, body, (self.clock)()).map_err(from_grant_error)?;
                let record = OauthRecord {
                    v: RECORD_VERSION,
                    access: token.access.expose().clone(),
                    refresh: token.refresh.as_ref().map(|refresh| refresh.expose().clone()),
                    expires_at: token.expires_at,
                    token_endpoint: token_endpoint.clone(),
                    client_id_ref: client_id_ref.clone(),
                    client_secret_ref: client_secret_ref.clone(),
                    client_auth: *client_auth,
                    assertion: None,
                    scopes: Vec::new(),
                    json_token: None,
                };
                self.persist_record(name, principal.as_deref(), token.label.as_deref(), record)?;
                Ok(CredentialStanding {
                    connected: true,
                    label: token.label,
                })
            }
            Exchange::Refresh {
                name,
                principal,
                token_endpoint,
                refresh,
                client_id_ref,
                client_secret_ref,
                client_auth,
                label,
            } => {
                let token = match parse_token_response(status, body, (self.clock)()) {
                    Ok(token) => token,
                    Err(error) => {
                        if error.kind == GrantErrorKind::InvalidGrant {
                            self.remove(name, principal.as_deref())
                                .map_err(|dropped| failure(ConnectFailureKind::Refused, dropped))?;
                        }
                        return Err(from_grant_error(error));
                    }
                };
                let label = token.label.or_else(|| label.clone());
                let record = OauthRecord {
                    v: RECORD_VERSION,
                    access: token.access.expose().clone(),
                    refresh: token
                        .refresh
                        .as_ref()
                        .map(|rotated| rotated.expose().clone())
                        .or_else(|| Some(refresh.clone())),
                    expires_at: token.expires_at,
                    token_endpoint: token_endpoint.clone(),
                    client_id_ref: client_id_ref.clone(),
                    client_secret_ref: client_secret_ref.clone(),
                    client_auth: *client_auth,
                    assertion: None,
                    scopes: Vec::new(),
                    json_token: None,
                };
                self.persist_record(name, principal.as_deref(), label.as_deref(), record)?;
                Ok(CredentialStanding {
                    connected: true,
                    label,
                })
            }
            Exchange::Assertion {
                name,
                principal,
                token_endpoint,
                spec,
                scopes,
            } => {
                let token = parse_token_response(status, body, (self.clock)()).map_err(from_grant_error)?;
                let record = OauthRecord {
                    v: RECORD_VERSION,
                    access: token.access.expose().clone(),
                    refresh: None,
                    expires_at: token.expires_at,
                    token_endpoint: token_endpoint.clone(),
                    client_id_ref: String::new(),
                    client_secret_ref: None,
                    client_auth: ClientAuth::None,
                    assertion: Some(spec.clone()),
                    scopes: scopes.clone(),
                    json_token: None,
                };
                self.persist_record(name, principal.as_deref(), None, record)?;
                Ok(CredentialStanding::of(true))
            }
            Exchange::JsonMint {
                name,
                principal,
                token_endpoint,
                spec,
                client_id_ref,
                client_secret_ref,
            } => {
                let token = parse_json_token_response(spec, false, status, body, (self.clock)()).map_err(from_grant_error)?;
                let record = OauthRecord {
                    v: RECORD_VERSION,
                    access: token.access.expose().clone(),
                    refresh: token.refresh.as_ref().map(|refresh| refresh.expose().clone()),
                    expires_at: token.expires_at,
                    token_endpoint: token_endpoint.clone(),
                    client_id_ref: client_id_ref.clone(),
                    client_secret_ref: Some(client_secret_ref.clone()),
                    client_auth: ClientAuth::None,
                    assertion: None,
                    scopes: Vec::new(),
                    json_token: Some(spec.clone()),
                };
                self.persist_record(name, principal.as_deref(), None, record)?;
                Ok(CredentialStanding::of(true))
            }
            Exchange::JsonRefresh {
                name,
                principal,
                token_endpoint,
                spec,
                refresh,
                client_id_ref,
                client_secret_ref,
            } => {
                let token = match parse_json_token_response(spec, true, status, body, (self.clock)()) {
                    Ok(token) => token,
                    Err(error) => {
                        if error.kind == GrantErrorKind::InvalidGrant {
                            self.remove(name, principal.as_deref())
                                .map_err(|dropped| failure(ConnectFailureKind::Refused, dropped))?;
                        }
                        return Err(from_grant_error(error));
                    }
                };
                let record = OauthRecord {
                    v: RECORD_VERSION,
                    access: token.access.expose().clone(),
                    refresh: token
                        .refresh
                        .as_ref()
                        .map(|rotated| rotated.expose().clone())
                        .or_else(|| Some(refresh.clone())),
                    expires_at: token.expires_at,
                    token_endpoint: token_endpoint.clone(),
                    client_id_ref: client_id_ref.clone(),
                    client_secret_ref: Some(client_secret_ref.clone()),
                    client_auth: ClientAuth::None,
                    assertion: None,
                    scopes: Vec::new(),
                    json_token: Some(spec.clone()),
                };
                self.persist_record(name, principal.as_deref(), None, record)?;
                Ok(CredentialStanding::of(true))
            }
        }
    }

    fn resolve_client(
        &self,
        id_ref: &str,
        secret_ref: Option<&str>,
        client_auth: ClientAuth,
    ) -> Result<(String, Option<String>), ConnectFailure> {
        let client_id = if id_ref.is_empty() && client_auth == ClientAuth::None {
            String::new()
        } else {
            self.resolve(id_ref, None).ok_or_else(|| {
                failure(
                    ConnectFailureKind::Refused,
                    format!("credential `{id_ref}` (the client id) is not stored"),
                )
            })?
        };
        let client_secret = match secret_ref {
            None => None,
            Some(name) => Some(self.resolve(name, None).ok_or_else(|| {
                failure(
                    ConnectFailureKind::Refused,
                    format!("credential `{name}` (the client secret) is not stored"),
                )
            })?),
        };
        Ok((client_id, client_secret))
    }

    fn persist_record(
        &self,
        name: &str,
        principal: Option<&str>,
        label: Option<&str>,
        record: OauthRecord,
    ) -> Result<(), ConnectFailure> {
        let mut plaintext = serde_json::to_string(&record)
            .map_err(|error| failure(ConnectFailureKind::Refused, error.to_string()))?;
        let sealed = RowBody {
            plaintext: &plaintext,
            kind: Some(OAUTH_KIND),
            last4: String::new(),
            value: record.access.clone(),
            oauth: Some(record),
        };
        let stored = self.store_row(OAUTH_SERVICE, name, principal, label, sealed);
        zeroize::Zeroize::zeroize(&mut plaintext);
        stored
            .map(|_| ())
            .map_err(|error| failure(ConnectFailureKind::Refused, error))
    }

    fn held_record(
        &self,
        name: &str,
        principal: Option<&str>,
    ) -> Option<(OauthRecord, Option<String>)> {
        let state = self.state.lock();
        let stored = state.by_identity.get(&storage_key(name, principal))?;
        Some((stored.oauth.clone()?, stored.label.clone()))
    }

    fn is_due(&self, name: &str, principal: Option<&str>) -> bool {
        let now = (self.clock)();
        self.state
            .lock()
            .by_identity
            .get(&storage_key(name, principal))
            .and_then(|stored| stored.oauth.as_ref())
            .and_then(|record| record.expires_at)
            .is_some_and(|expires_at| expires_at <= now)
    }

    fn flight(&self, name: &str, principal: Option<&str>) -> Arc<Mutex<()>> {
        self.flights
            .lock()
            .entry(storage_key(name, principal))
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// The access token of `name` for `principal`, refreshed first when it is past its `expiresAt`. Concurrent resolves of
    /// one credential share a single refresh. `None` when nothing is held, when the row has no refresh token to renew
    /// with, or when the provider refused the refresh (`invalid_grant`, which drops the row); a transport or other
    /// failure keeps the row and is returned.
    pub fn resolve_fresh(
        &self,
        name: &str,
        principal: Option<&str>,
        transport: Option<&dyn CredentialTransport>,
    ) -> Result<Option<String>, ConnectFailure> {
        self.ensure_warm()
            .map_err(|error| failure(ConnectFailureKind::Unavailable, error))?;
        if !self.is_due(name, principal) {
            return Ok(self.resolve(name, principal));
        }
        let flight = self.flight(name, principal);
        let _turn = flight.lock();
        let pending = match self.refresh_begin(name, principal)? {
            RefreshStart::Resolved(value) => return Ok(value),
            RefreshStart::Exchange(pending) => pending,
        };
        let transport = transport.ok_or_else(|| {
            failure(
                ConnectFailureKind::Unavailable,
                "credential-connect: this host has no transport to refresh the credential",
            )
        })?;
        match self.send_exchange(pending, transport) {
            Ok(_) => Ok(self.resolve(name, principal)),
            Err(error) if error.kind == ConnectFailureKind::InvalidGrant => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The first half of [`Self::resolve_fresh`]: the credential's answer when nothing needs refreshing (or nothing can
    /// be), else the refresh request to send. The caller serializes concurrent begins of one credential.
    pub fn refresh_begin(&self, name: &str, principal: Option<&str>) -> Result<RefreshStart, ConnectFailure> {
        self.ensure_warm()
            .map_err(|error| failure(ConnectFailureKind::Unavailable, error))?;
        if !self.is_due(name, principal) {
            return Ok(RefreshStart::Resolved(self.resolve(name, principal)));
        }
        let Some((record, label)) = self.held_record(name, principal) else {
            return Ok(RefreshStart::Resolved(None));
        };
        if let Some(spec) = &record.assertion {
            return self
                .assertion_pending(name, principal, &record.token_endpoint, spec, &record.scopes)
                .map(RefreshStart::Exchange);
        }
        if let Some(spec) = &record.json_token {
            let (Some(refresh), Some(secret_ref)) = (record.refresh.clone(), record.client_secret_ref.as_deref()) else {
                return Ok(RefreshStart::Resolved(None));
            };
            if spec.refresh_endpoint.is_none() {
                return Ok(RefreshStart::Resolved(None));
            }
            return self
                .json_refresh_pending(
                    name,
                    principal,
                    &record.token_endpoint,
                    spec,
                    refresh,
                    (&record.client_id_ref, secret_ref),
                )
                .map(RefreshStart::Exchange);
        }
        let Some(refresh) = record.refresh.clone() else {
            return Ok(RefreshStart::Resolved(None));
        };
        let refs = ClientRefs {
            client_id: &record.client_id_ref,
            client_secret: record.client_secret_ref.as_deref(),
            client_auth: record.client_auth,
        };
        self.refresh_pending(name, principal, &record.token_endpoint, refresh, refs, label)
            .map(RefreshStart::Exchange)
    }

    fn refresh_grant_begin(&self, grant: &CredentialGrant) -> Result<PendingExchange, ConnectFailure> {
        if let Some(source) = grant.refresh_credential.as_deref() {
            if source == grant.name {
                return Err(failure(
                    ConnectFailureKind::Refused,
                    "credential-connect: the refresh credential must differ from the credential it connects",
                ));
            }
            let refresh = self
                .resolve(source, None)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    failure(
                        ConnectFailureKind::InvalidGrant,
                        format!("credential `{source}` (the refresh token) is not stored"),
                    )
                })?;
            let refs = ClientRefs {
                client_id: &grant.client_id,
                client_secret: grant.client_secret.as_deref(),
                client_auth: grant.client_auth,
            };
            return self.refresh_pending(
                &grant.name,
                grant.principal.as_deref(),
                &grant.token_endpoint,
                refresh,
                refs,
                None,
            );
        }
        let held = self.held_record(&grant.name, grant.principal.as_deref());
        let Some((record, label)) = held else {
            return Err(failure(
                ConnectFailureKind::InvalidGrant,
                format!("credential `{}` holds no connection to refresh", grant.name),
            ));
        };
        if record.assertion.is_some() || record.json_token.is_some() {
            return Err(failure(
                ConnectFailureKind::Refused,
                "credential-connect: the credential was not granted by an OAuth refresh-token grant",
            ));
        }
        let Some(refresh) = record.refresh.clone() else {
            return Err(failure(
                ConnectFailureKind::InvalidGrant,
                format!("credential `{}` holds no refresh token", grant.name),
            ));
        };
        if grant.token_endpoint != record.token_endpoint {
            return Err(failure(
                ConnectFailureKind::Refused,
                "credential-connect: the token endpoint is not the one the credential was granted at",
            ));
        }
        let refs = ClientRefs {
            client_id: &grant.client_id,
            client_secret: grant.client_secret.as_deref(),
            client_auth: grant.client_auth,
        };
        self.refresh_pending(
            &grant.name,
            grant.principal.as_deref(),
            &record.token_endpoint,
            refresh,
            refs,
            label,
        )
    }

    fn refresh_pending(
        &self,
        name: &str,
        principal: Option<&str>,
        token_endpoint: &str,
        refresh: String,
        refs: ClientRefs<'_>,
        label: Option<String>,
    ) -> Result<PendingExchange, ConnectFailure> {
        let (client_id, client_secret) = self.resolve_client(refs.client_id, refs.client_secret, refs.client_auth)?;
        let request = refresh_token_request(&RefreshExchange {
            refresh_token: &refresh,
            client_id: &client_id,
            client_secret: client_secret.as_deref(),
            client_auth: refs.client_auth,
        })
        .map_err(from_grant_error)?;
        Ok(PendingExchange {
            endpoint: token_endpoint.to_string(),
            headers: request.headers,
            body: request.body,
            ticket: ExchangeTicket {
                exchange: Exchange::Refresh {
                    name: name.to_string(),
                    principal: principal.map(str::to_string),
                    token_endpoint: token_endpoint.to_string(),
                    refresh,
                    client_id_ref: refs.client_id.to_string(),
                    client_secret_ref: refs.client_secret.map(str::to_string),
                    client_auth: refs.client_auth,
                    label,
                },
            },
        })
    }
}

struct ClientRefs<'a> {
    client_id: &'a str,
    client_secret: Option<&'a str>,
    client_auth: ClientAuth,
}
