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
    authorization_code_request, parse_token_response, refresh_token_request, ClientAuth,
    CodeExchange, GrantError, GrantErrorKind, RefreshExchange,
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
    client_id_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_secret_ref: Option<String>,
    client_auth: ClientAuth,
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
        if grant.grant == "refresh_token" {
            return self.refresh_grant(grant, transport);
        }
        if grant.grant != "authorization_code" {
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
        let (client_id, client_secret) =
            self.resolve_client(&grant.client_id, grant.client_secret.as_deref())?;
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
        let (status, body) = transport
            .post_form(&grant.token_endpoint, &request.headers, &request.body)
            .map_err(|error| failure(ConnectFailureKind::Transport, error.0))?;
        let token =
            parse_token_response(status, &body, (self.clock)()).map_err(from_grant_error)?;
        let record = OauthRecord {
            v: RECORD_VERSION,
            access: token.access.expose().clone(),
            refresh: token
                .refresh
                .as_ref()
                .map(|refresh| refresh.expose().clone()),
            expires_at: token.expires_at,
            token_endpoint: grant.token_endpoint.clone(),
            client_id_ref: grant.client_id.clone(),
            client_secret_ref: grant.client_secret.clone(),
            client_auth: grant.client_auth,
        };
        self.persist_record(
            &grant.name,
            grant.principal.as_deref(),
            token.label.as_deref(),
            record,
        )?;
        Ok(CredentialStanding {
            connected: true,
            label: token.label,
        })
    }

    fn resolve_client(
        &self,
        id_ref: &str,
        secret_ref: Option<&str>,
    ) -> Result<(String, Option<String>), ConnectFailure> {
        let client_id = self.resolve(id_ref, None).ok_or_else(|| {
            failure(
                ConnectFailureKind::Refused,
                format!("credential `{id_ref}` (the client id) is not stored"),
            )
        })?;
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
        if !self.is_due(name, principal) {
            return Ok(self.resolve(name, principal));
        }
        let Some((record, label)) = self.held_record(name, principal) else {
            return Ok(None);
        };
        let Some(refresh) = record.refresh.clone() else {
            return Ok(None);
        };
        let transport = transport.ok_or_else(|| {
            failure(
                ConnectFailureKind::Unavailable,
                "credential-connect: this host has no transport to refresh the credential",
            )
        })?;
        let refs = ClientRefs {
            client_id: &record.client_id_ref,
            client_secret: record.client_secret_ref.as_deref(),
            client_auth: record.client_auth,
        };
        match self.exchange_refresh(
            name,
            principal,
            &record.token_endpoint,
            &refresh,
            refs,
            label,
            transport,
        ) {
            Ok(_) => Ok(self.resolve(name, principal)),
            Err(error) if error.kind == ConnectFailureKind::InvalidGrant => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn refresh_grant(
        &self,
        grant: &CredentialGrant,
        transport: &dyn CredentialTransport,
    ) -> Result<CredentialStanding, ConnectFailure> {
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
        let flight = self.flight(&grant.name, grant.principal.as_deref());
        let _turn = flight.lock();
        let held = self.held_record(&grant.name, grant.principal.as_deref());
        let Some((record, label)) = held else {
            return Err(failure(
                ConnectFailureKind::InvalidGrant,
                format!("credential `{}` holds no connection to refresh", grant.name),
            ));
        };
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
        let label = self.exchange_refresh(
            &grant.name,
            grant.principal.as_deref(),
            &record.token_endpoint,
            &refresh,
            refs,
            label,
            transport,
        )?;
        Ok(CredentialStanding {
            connected: true,
            label,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn exchange_refresh(
        &self,
        name: &str,
        principal: Option<&str>,
        token_endpoint: &str,
        refresh: &str,
        refs: ClientRefs<'_>,
        label: Option<String>,
        transport: &dyn CredentialTransport,
    ) -> Result<Option<String>, ConnectFailure> {
        let (client_id, client_secret) = self.resolve_client(refs.client_id, refs.client_secret)?;
        let request = refresh_token_request(&RefreshExchange {
            refresh_token: refresh,
            client_id: &client_id,
            client_secret: client_secret.as_deref(),
            client_auth: refs.client_auth,
        })
        .map_err(from_grant_error)?;
        let (status, body) = transport
            .post_form(token_endpoint, &request.headers, &request.body)
            .map_err(|error| failure(ConnectFailureKind::Transport, error.0))?;
        let token = match parse_token_response(status, &body, (self.clock)()) {
            Ok(token) => token,
            Err(error) => {
                if error.kind == GrantErrorKind::InvalidGrant {
                    self.remove(name, principal)
                        .map_err(|dropped| failure(ConnectFailureKind::Refused, dropped))?;
                }
                return Err(from_grant_error(error));
            }
        };
        let label = token.label.or(label);
        let record = OauthRecord {
            v: RECORD_VERSION,
            access: token.access.expose().clone(),
            refresh: token
                .refresh
                .as_ref()
                .map(|rotated| rotated.expose().clone())
                .or_else(|| Some(refresh.to_string())),
            expires_at: token.expires_at,
            token_endpoint: token_endpoint.to_string(),
            client_id_ref: refs.client_id.to_string(),
            client_secret_ref: refs.client_secret.map(str::to_string),
            client_auth: refs.client_auth,
        };
        self.persist_record(name, principal, label.as_deref(), record)?;
        Ok(label)
    }
}

struct ClientRefs<'a> {
    client_id: &'a str,
    client_secret: Option<&'a str>,
    client_auth: ClientAuth,
}
