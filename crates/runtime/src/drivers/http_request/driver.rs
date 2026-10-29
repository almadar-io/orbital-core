//! The sans-I/O step machine that drives one `http` call.

use super::*;

// --- the step machine -------------------------------------------------------------------------------------


/// Private credentials and built requests, never serialized or printed with the driver.
#[derive(Clone, PartialEq, Default)]
pub struct HeldSecrets {
    pub(super) credentials: Vec<(CredentialSlot, String)>,
    pub(super) grant_request: Option<HttpRequest>,
    pub(super) resource_request: Option<HttpRequest>,
    pub(super) oauth: OAuthHeld,
}

impl std::fmt::Debug for HeldSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.credentials.iter().map(|(slot, _)| &slot.name)).finish()
    }
}

impl HeldSecrets {
    pub(super) fn get(&self, slot: &CredentialSlot) -> Option<&str> {
        self.credentials.iter().find(|(held, _)| held == slot).map(|(_, v)| v.as_str())
    }

    pub(super) fn hold(&mut self, slot: CredentialSlot, value: String) {
        self.credentials.retain(|(held, _)| held != &slot);
        self.credentials.push((slot, value));
    }

    pub(super) fn forget(&mut self, slot: &CredentialSlot) {
        self.credentials.retain(|(held, _)| held != slot);
    }

    pub fn is_empty(&self) -> bool {
        self.credentials.is_empty()
            && self.grant_request.is_none()
            && self.resource_request.is_none()
            && self.oauth.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub(super) enum Phase {
    Credentials { pending: Vec<CredentialSlot>, awaiting: Option<CredentialSlot> },
    GrantSecrets { pending: Vec<CredentialSlot>, awaiting: Option<CredentialSlot> },
    GrantReply,
    GrantRotated,
    GrantStored,
    Boundary,
    WebPushEntropy,
    OAuth,
    Reply,
    Finished,
}

/// An `oauth-client` call's declaration with its resolved credentials.
pub(super) struct OAuthCall {
    pub(super) op: OAuthOp,
    pub(super) client: OAuthClientDecl,
    pub(super) clients: Vec<OAuthClientDecl>,
    pub(super) inputs: OAuthInputs,
    pub(super) id: String,
    pub(super) secret: String,
}

/// One `http` call as a pure step machine: `start` yields the first ask, `resume` feeds each answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpCallDriver {
    pub(super) spec: HttpCallSpec,
    #[serde(default)]
    pub(super) now_ms: Option<f64>,
    #[serde(default)]
    pub(super) webpush_body: Option<Vec<u8>>,
    pub(super) egress: Vec<String>,
    pub(super) key: Option<String>,
    pub(super) phase: Phase,
    #[serde(skip)]
    pub(super) secrets: HeldSecrets,
    /// Where an embedding host (wasm) keeps this call's [`HeldSecrets`] between steps; opaque to the driver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) vault: Option<u64>,
    pub(super) oauth_miss: bool,
    pub(super) granted: bool,
    pub(super) refreshed: bool,
    pub(super) pages: u32,
    pub(super) items: Vec<Value>,
    pub(super) page_token: Option<String>,
    #[serde(default)]
    pub(super) boundary: Option<String>,
    /// Retries spent on the request in flight; a new page starts again at 0.
    #[serde(default)]
    pub(super) retries: u32,
    #[serde(default)]
    pub(super) grant_retries: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) pending_delay_ms: Option<u64>,
    /// The lifetime of the access token waiting behind a rotation; the token itself is a held secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) pending_ttl_ms: Option<u64>,
    /// The `@user` id the kernel sets for an `oauth-client` call; never the program.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) oauth: Option<OAuthFlow>,
}

/// A call with no `key` answers with no `key` field: a field without `!` is absent, never null.
pub(crate) fn with_key(mut payload: Value, key: &Option<String>) -> Value {
    if let (Value::Object(map), Some(key)) = (&mut payload, key) {
        map.insert("key".into(), Value::string(key.clone()));
    }
    payload
}

#[cfg(test)]
mod with_key_tests {
    use super::*;

    #[test]
    fn an_answer_to_a_call_without_a_key_has_no_key_field() {
        let answer = with_key(Value::object(orbital_core_base::value::ValueMap::default()), &None);
        assert!(answer.get_path("key").is_none(), "absent, not null: {answer:?}");
    }

    #[test]
    fn control_an_answer_to_a_keyed_call_carries_its_key() {
        let answer = with_key(Value::object(orbital_core_base::value::ValueMap::default()), &Some("k1".to_string()));
        assert_eq!(answer.get_path("key").and_then(|k| k.as_str().map(str::to_string)), Some("k1".to_string()));
    }
}

pub(super) const BOUNDARY_BYTES: u32 = 16;

impl crate::driver::Driver for HttpCallDriver {
    fn resume(&mut self, answer: HttpAnswer) -> HttpStep {
        HttpCallDriver::resume(self, answer)
    }

    fn http_continuation(&self) -> Option<Box<HttpCallDriver>> {
        Some(Box::new(self.clone()))
    }
}

impl HttpCallDriver {
    #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
    pub(crate) fn principal(&self) -> Option<&str> {
        self.principal.as_deref()
    }

    pub fn start(spec: HttpCallSpec, egress: Vec<String>, key: Option<String>) -> (HttpCallDriver, HttpStep) {
        Self::start_stamped(spec, egress, key, None)
    }

    pub fn start_stamped(spec: HttpCallSpec, egress: Vec<String>, key: Option<String>, now_ms: Option<f64>) -> (HttpCallDriver, HttpStep) {
        Self::start_as(spec, egress, key, now_ms, None)
    }

    /// [`Self::start_stamped`] for the signed-in user `principal`: an `oauth-client` call with no credential to
    /// ask (a mock client) begins its flow inside `start`, so the principal must already be there.
    pub fn start_as(
        spec: HttpCallSpec,
        egress: Vec<String>,
        key: Option<String>,
        now_ms: Option<f64>,
        principal: Option<String>,
    ) -> (HttpCallDriver, HttpStep) {
        let pending: Vec<CredentialSlot> = credential_slots_needed(&spec).into_iter().rev().collect();
        let first_delay_ms = spec.delay_ms;
        let mut driver = HttpCallDriver {
            spec,
            now_ms,
            webpush_body: None,
            egress,
            key,
            phase: Phase::Credentials {
                pending,
                awaiting: None,
            },
            secrets: HeldSecrets::default(),
            vault: None,
            oauth_miss: false,
            granted: false,
            refreshed: false,
            pages: 0,
            items: Vec::new(),
            page_token: None,
            boundary: None,
            retries: 0,
            grant_retries: 0,
            pending_delay_ms: first_delay_ms,
            pending_ttl_ms: None,
            principal,
            oauth: None,
        };
        let step = if origin_allowed(&driver.spec.origin, &driver.egress) || driver.spec.origin == OAuthEndpointKind::target() {
            driver.advance()
        } else {
            let origin = driver.spec.origin.clone();
            driver.refuse(HttpRefusal::EgressDenied(origin))
        };
        (driver, step)
    }

    /// The finished step of a call refused before it could start (it did not parse): the refusal's failure payload
    /// with the call's `key`.
    pub fn refused(refusal: &HttpRefusal, key: Option<&str>) -> HttpStep {
        HttpStep::Done(Err(with_key(refusal_payload(refusal), &key.map(str::to_string))))
    }

    /// Hands the held credential values to an embedding host that keeps them outside the serialized driver.
    pub fn take_secrets(&mut self) -> HeldSecrets {
        std::mem::take(&mut self.secrets)
    }

    pub fn restore_secrets(&mut self, secrets: HeldSecrets) {
        self.secrets = secrets;
    }

    pub fn vault(&self) -> Option<u64> {
        self.vault
    }

    /// The signed-in user an `oauth-client` call acts for. Set by the kernel before the call's first answer.
    pub fn set_principal(&mut self, principal: Option<String>) {
        self.principal = principal;
    }

    pub fn set_vault(&mut self, vault: Option<u64>) {
        self.vault = vault;
    }

    /// Frames a stream chunk; each event carries the call's `key`.
    pub fn stream_events(&self, chunk: &str, carry: &mut String) -> Vec<Value> {
        match self.spec.stream {
            Some(framing) => frame_stream(framing, self.key.as_deref(), chunk, carry),
            None => Vec::new(),
        }
    }

    pub fn stream_finish(&self, carry: &mut String) -> Vec<Value> {
        match self.spec.stream {
            Some(framing) => frame_stream_finish(framing, self.key.as_deref(), carry),
            None => Vec::new(),
        }
    }

    pub fn resume(&mut self, answer: HttpAnswer) -> HttpStep {
        match (self.phase.clone(), answer) {
            (Phase::Credentials { pending, awaiting }, HttpAnswer::Secret(result)) => {
                let Some(slot) = awaiting else {
                    return self.unexpected();
                };
                match result {
                    Ok(Some(value)) => self.secrets.hold(slot, value),
                    Ok(None) if matches!(self.spec.auth, Some(HttpAuth::OAuthClient { .. })) => {
                        return self.fail_oauth(OAuthFailureReason::UnconfiguredClient)
                    }
                    Ok(None) if self.cache_slot().as_ref() == Some(&slot) && !self.granted => self.oauth_miss = true,
                    Ok(None) => return self.refuse(HttpRefusal::CredentialMissing(slot.name)),
                    Err(message) => return self.fail(TransportError::Failed(message)),
                }
                self.phase = Phase::Credentials { pending, awaiting: None };
                self.advance()
            }
            (Phase::GrantSecrets { pending, awaiting }, HttpAnswer::Secret(result)) => {
                let Some(slot) = awaiting else {
                    return self.unexpected();
                };
                match result {
                    Ok(Some(value)) => self.secrets.hold(slot, value),
                    Ok(None) => return self.refuse(HttpRefusal::CredentialMissing(slot.name)),
                    Err(message) => return self.fail(TransportError::Failed(message)),
                }
                self.phase = Phase::GrantSecrets { pending, awaiting: None };
                self.advance()
            }
            (Phase::GrantReply, HttpAnswer::Http(Err(e))) => self.on_transport_error(e),
            (Phase::GrantReply, HttpAnswer::Http(Ok(resp))) => self.on_grant_reply(resp),
            (Phase::GrantRotated, HttpAnswer::SecretRotate(Ok(()))) => self.store_granted_token(),
            (Phase::GrantRotated, HttpAnswer::SecretRotate(Err(message))) => {
                self.fail(TransportError::Failed(message))
            }
            (Phase::GrantStored, HttpAnswer::SecretSet(Ok(()))) => self.send_page(),
            (Phase::GrantStored, HttpAnswer::SecretSet(Err(message))) => {
                self.fail(TransportError::Failed(message))
            }
            (Phase::Boundary, HttpAnswer::Random(Ok(bytes))) if bytes.len() == BOUNDARY_BYTES as usize => {
                self.boundary = Some(format!("almadar-{}", codec::hex(&bytes)));
                self.send_page()
            }
            (Phase::Boundary, HttpAnswer::Random(Ok(bytes))) => self.fail(TransportError::Failed(format!(
                "the host answered {} random bytes, asked for {BOUNDARY_BYTES}",
                bytes.len()
            ))),
            (Phase::Boundary, HttpAnswer::Random(Err(message))) => self.fail(TransportError::Failed(message)),
            (Phase::WebPushEntropy, HttpAnswer::Random(Ok(bytes))) if bytes.len() == 48 => {
                let HttpBodySpec::WebPush(push) = &self.spec.body else { return self.unexpected() };
                let mut key = [0;32]; key.copy_from_slice(&bytes[..32]);
                let mut salt = [0;16]; salt.copy_from_slice(&bytes[32..]);
                if crate::webpush::ephemeral_private_key(&key).is_err() { return HttpStep::Ask(HttpAsk::Random { bytes:48 }) }
                match crate::webpush::encrypt_aes128gcm(&push.p256dh,&push.auth,push.payload.as_bytes(),&key,&salt) {
                    Ok(body) => { self.webpush_body = Some(body); self.send_page() }
                    Err(error) => self.refuse(HttpRefusal::Invalid(error.0)),
                }
            }
            (Phase::WebPushEntropy, HttpAnswer::Random(Ok(bytes))) => self.fail(TransportError::Failed(format!("the host answered {} random bytes, asked for 48",bytes.len()))),
            (Phase::WebPushEntropy, HttpAnswer::Random(Err(message))) => self.fail(TransportError::Failed(message)),
            (Phase::OAuth, HttpAnswer::OAuth(result)) => self.resume_oauth(FlowAnswer::OAuth(result)),
            (Phase::OAuth, HttpAnswer::Http(result)) => self.resume_oauth(FlowAnswer::Http(result)),
            (Phase::OAuth, HttpAnswer::Random(result)) => self.resume_oauth(FlowAnswer::Random(result)),
            (Phase::OAuth, HttpAnswer::Secret(result)) => self.resume_oauth(FlowAnswer::Secret(result)),
            (Phase::Reply, HttpAnswer::Http(Err(e))) => self.on_transport_error(e),
            (Phase::Reply, HttpAnswer::Http(Ok(resp))) => self.on_reply(resp),
            (Phase::Reply, HttpAnswer::Beacon(result)) => self.on_beacon(result),
            _ => self.unexpected(),
        }
    }

    pub(super) fn exchange(&self) -> Option<&TokenExchange> {
        match &self.spec.auth {
            Some(HttpAuth::TokenExchange(ex)) => Some(ex),
            _ => None,
        }
    }

    pub(super) fn cache_slot(&self) -> Option<CredentialSlot> {
        self.exchange().map(|ex| ex.store_as.slot())
    }

    pub(super) fn unexpected(&mut self) -> HttpStep {
        self.fail(TransportError::Failed("unexpected answer for the http call".to_string()))
    }

    pub(super) fn done(&mut self, result: Result<Value, Value>) -> HttpStep {
        self.phase = Phase::Finished;
        self.secrets = HeldSecrets::default();
        HttpStep::Done(result.map(|v| with_key(v, &self.key)).map_err(|v| with_key(v, &self.key)))
    }

    pub(super) fn fail(&mut self, err: TransportError) -> HttpStep {
        let payload = self.with_phase(failure_payload(&err));
        self.done(Err(payload))
    }

    pub(super) fn fail_grant(&mut self, err: TransportError, rejected: Option<&HttpResponse>) -> HttpStep {
        let payload = self.with_phase(failure_payload(&err));
        let payload = match rejected {
            Some(resp) => {
                let payload = set_field(payload, "grantStatus", Value::Number(f64::from(resp.status)));
                set_field(payload, "grantBody", Value::string(resp.text().into_owned()))
            }
            None => payload,
        };
        self.done(Err(payload))
    }

    /// Names the leg a failure belongs to: the grant exchange or the authored request.
    pub(super) fn with_phase(&self, payload: Value) -> Value {
        let phase = match self.phase {
            Phase::GrantReply | Phase::GrantRotated | Phase::GrantStored => "grant",
            Phase::Reply => "request",
            _ => return payload,
        };
        set_field(payload, "phase", Value::string(phase.to_string()))
    }

    pub(super) fn refuse(&mut self, refusal: HttpRefusal) -> HttpStep {
        self.done(Err(refusal_payload(&refusal)))
    }

    pub(super) fn advance(&mut self) -> HttpStep {
        match self.phase.clone() {
            Phase::Credentials { mut pending, .. } => match pending.pop() {
                Some(slot) => {
                    self.phase = Phase::Credentials {
                        pending,
                        awaiting: Some(slot.clone()),
                    };
                    HttpStep::Ask(HttpAsk::Secret {
                        name: slot.name,
                        scope: slot.scope,
                    })
                }
                None if self.oauth_miss => self.begin_grant(),
                None => self.send_page(),
            },
            Phase::GrantSecrets { mut pending, .. } => match pending.pop() {
                Some(slot) => {
                    self.phase = Phase::GrantSecrets {
                        pending,
                        awaiting: Some(slot.clone()),
                    };
                    HttpStep::Ask(HttpAsk::Secret {
                        name: slot.name,
                        scope: slot.scope,
                    })
                }
                None => self.send_grant(self.pending_delay_ms),
            },
            _ => self.unexpected(),
        }
    }

    pub(super) fn missing_exchange_slots(&self) -> Vec<CredentialSlot> {
        let Some(ex) = self.exchange() else {
            return Vec::new();
        };
        let mut pending: Vec<CredentialSlot> = exchange_slots(ex)
            .into_iter()
            .filter(|slot| self.secrets.get(slot).is_none())
            .collect();
        pending.reverse();
        pending
    }

    pub(super) fn begin_grant(&mut self) -> HttpStep {
        if self.exchange().is_none() {
            return self.unexpected();
        }
        self.grant_retries = 0;
        self.pending_delay_ms = None;
        self.secrets.grant_request = None;
        self.secrets.resource_request = None;
        let pending = self.missing_exchange_slots();
        self.phase = Phase::GrantSecrets { pending, awaiting: None };
        self.advance()
    }

    pub(super) fn lookup(&self) -> impl Fn(&CredentialSlot) -> Option<String> + '_ {
        move |slot| self.secrets.get(slot).map(str::to_string)
    }

    pub(super) fn send_grant(&mut self, delay_ms: Option<u64>) -> HttpStep {
        self.pending_delay_ms = delay_ms;
        let Some(auth) = self.spec.auth.clone() else {
            return self.unexpected();
        };
        let pending = self.missing_exchange_slots();
        if !pending.is_empty() {
            self.phase = Phase::GrantSecrets { pending, awaiting: None };
            return self.advance();
        }
        let built = match self.secrets.grant_request.clone() {
            Some(request) => Ok(request),
            None => token_exchange_request(&auth, &self.lookup()),
        };
        let request = match built {
            Ok(request) => request,
            Err(refusal) => return self.refuse(refusal),
        };
        if !origin_allowed(&request.url, &self.egress) {
            let url = request.url;
            return self.refuse(HttpRefusal::EgressDenied(url));
        }
        self.secrets.grant_request = Some(request.clone());
        self.phase = Phase::GrantReply;
        HttpStep::Ask(HttpAsk::Http {
            request,
            delay_ms,
        })
    }

    pub(super) fn on_grant_reply(&mut self, resp: HttpResponse) -> HttpStep {
        if let Some(step) = self.retry_step(RetryOutcome::Status(resp.status)) {
            return step;
        }
        let Some((access_field, expires_field, margin, store)) = self.exchange().map(|ex| {
            (
                ex.access_field.clone(),
                ex.expires_field.clone(),
                ex.expires_margin_ms,
                ex.store_as.clone(),
            )
        }) else {
            return self.unexpected();
        };
        match parse_token_reply(&resp, &access_field, expires_field.as_deref(), margin) {
            Err(message) => {
                let rejected = (!resp.is_success()).then_some(&resp);
                self.fail_grant(TransportError::Failed(format!("token exchange: {message}")), rejected)
            }
            Ok((token, ttl_ms)) => {
                let rotate = self.exchange().and_then(|ex| ex.rotate.clone());
                let replacement = match &rotate {
                    Some(rotate) => match parse_rotated_token(&resp, &rotate.field) {
                        Ok(value) => Some(value),
                        Err(message) => {
                            return self.fail_grant(TransportError::Failed(format!("token exchange: {message}")), None)
                        }
                    },
                    None => None,
                };
                self.secrets.hold(store.slot(), token);
                self.granted = true;
                self.oauth_miss = false;
                self.pending_delay_ms = None;
                match (rotate, replacement) {
                    (Some(rotate), Some(value)) => {
                        self.secrets.hold(rotate.credential.clone(), value.clone());
                        self.pending_ttl_ms = Some(ttl_ms);
                        self.phase = Phase::GrantRotated;
                        HttpStep::Ask(HttpAsk::SecretRotate {
                            name: rotate.credential.name,
                            value,
                            scope: rotate.credential.scope,
                        })
                    }
                    _ => {
                        self.pending_ttl_ms = Some(ttl_ms);
                        self.store_granted_token()
                    }
                }
            }
        }
    }

    /// Caches the held access token under `storeAs` (after any rotation has been written).
    pub(super) fn store_granted_token(&mut self) -> HttpStep {
        let Some(store) = self.exchange().map(|ex| ex.store_as.clone()) else {
            return self.unexpected();
        };
        let (Some(token), Some(ttl_ms)) = (self.secrets.get(&store.slot()).map(str::to_string), self.pending_ttl_ms.take()) else {
            return self.unexpected();
        };
        self.phase = Phase::GrantStored;
        HttpStep::Ask(HttpAsk::SecretSet {
            name: store.name,
            value: token,
            ttl_ms,
            scope: store.scope,
        })
    }

    pub(super) fn send_page(&mut self) -> HttpStep {
        self.send_request(self.pending_delay_ms)
    }

    pub(super) fn send_request(&mut self, delay_ms: Option<u64>) -> HttpStep {
        self.pending_delay_ms = delay_ms;
        let pending: Vec<CredentialSlot> = credential_slots_needed(&self.spec)
            .into_iter()
            .rev()
            .filter(|slot| self.secrets.get(slot).is_none())
            .collect();
        if !pending.is_empty() {
            self.phase = Phase::Credentials { pending, awaiting: None };
            return self.advance();
        }
        if matches!(self.spec.auth, Some(HttpAuth::OAuthClient { .. })) {
            return self.begin_oauth();
        }
        if matches!(self.spec.body, HttpBodySpec::Multipart { .. }) && self.boundary.is_none() {
            self.phase = Phase::Boundary;
            return HttpStep::Ask(HttpAsk::Random { bytes: BOUNDARY_BYTES });
        }
        if matches!(self.spec.body, HttpBodySpec::WebPush(_)) && self.webpush_body.is_none() {
            self.phase = Phase::WebPushEntropy;
            return HttpStep::Ask(HttpAsk::Random { bytes:48 });
        }
        let built = match self.secrets.resource_request.clone() {
            Some(request) => Ok(request),
            None => build_request_with_metadata(
                &self.spec,
                &self.lookup(),
                &self.egress,
                self.page_token.as_deref(),
                self.boundary.as_deref(),
                self.now_ms,
                self.webpush_body.as_deref(),
            ),
        };
        let request = match built {
            Ok(request) => request,
            Err(refusal) => return self.refuse(refusal),
        };
        self.secrets.resource_request = Some(request.clone());
        if self.spec.presign.is_some() {
            let mut payload = ValueMap::default();
            payload.insert("url".into(), Value::string(request.url));
            return self.done(Ok(Value::object(payload)));
        }
        self.phase = Phase::Reply;
        if self.spec.delivery == HttpDelivery::Beacon {
            return HttpStep::Ask(HttpAsk::Beacon { request });
        }
        match self.spec.stream {
            Some(framing) => HttpStep::Ask(HttpAsk::HttpStream { request, framing }),
            None => HttpStep::Ask(HttpAsk::Http { request, delay_ms }),
        }
    }

    pub(super) fn fail_oauth(&mut self, reason: OAuthFailureReason) -> HttpStep {
        self.done(Err(oauth_failure_payload(reason.message(), reason.code(), reason.as_str())))
    }

    /// The call's declaration and resolved credentials; `selected` is the `clients` entry a taken grant chose.
    pub(super) fn oauth_env(&self, selected: Option<usize>) -> Option<OAuthCall> {
        let Some(HttpAuth::OAuthClient { op, client, clients, inputs }) = &self.spec.auth else {
            return None;
        };
        let effective = match selected {
            Some(index) => clients.get(index)?,
            None => client,
        };
        let (id, secret) = if effective.mode == OAuthMode::Mock {
            (String::new(), String::new())
        } else if !clients.is_empty() {
            (
                self.secrets.oauth.client_id.clone().unwrap_or_default(),
                self.secrets.oauth.client_secret.clone().unwrap_or_default(),
            )
        } else {
            let id = self.secrets.get(&effective.client_id)?.to_string();
            let secret = effective
                .client_secret
                .as_ref()
                .and_then(|slot| self.secrets.get(slot))
                .unwrap_or_default()
                .to_string();
            (id, secret)
        };
        Some(OAuthCall { op: *op, client: effective.clone(), clients: clients.clone(), inputs: inputs.clone(), id, secret })
    }

    pub(super) fn begin_oauth(&mut self) -> HttpStep {
        let Some(call) = self.oauth_env(None) else {
            return self.fail_oauth(OAuthFailureReason::UnconfiguredClient);
        };
        if call.clients.is_empty() && call.client.mode == OAuthMode::Real && call.client.client_secret.is_some() && call.secret.is_empty() {
            return self.fail_oauth(OAuthFailureReason::UnconfiguredClient);
        }
        self.phase = Phase::OAuth;
        let url = format!("{}{}", self.spec.origin, self.spec.path);
        let env = OAuthEnv {
            op: call.op,
            client: &call.client,
            clients: &call.clients,
            inputs: &call.inputs,
            client_id: &call.id,
            client_secret: &call.secret,
            egress: &self.egress,
            authored_url: &url,
        };
        let mut flow = OAuthFlow::new(self.principal.clone());
        let step = flow.start(&env, &mut self.secrets.oauth);
        self.oauth = Some(flow);
        self.oauth_step(step)
    }

    pub(super) fn resume_oauth(&mut self, answer: FlowAnswer) -> HttpStep {
        let Some(mut flow) = self.oauth.take() else {
            return self.unexpected();
        };
        let Some(call) = self.oauth_env(flow.selected()) else {
            return self.fail_oauth(OAuthFailureReason::StoreUnavailable);
        };
        let url = format!("{}{}", self.spec.origin, self.spec.path);
        let env = OAuthEnv {
            op: call.op,
            client: &call.client,
            clients: &call.clients,
            inputs: &call.inputs,
            client_id: &call.id,
            client_secret: &call.secret,
            egress: &self.egress,
            authored_url: &url,
        };
        let step = flow.resume(&env, &mut self.secrets.oauth, answer);
        self.oauth = Some(flow);
        self.oauth_step(step)
    }

    pub(super) fn oauth_step(&mut self, step: FlowStep) -> HttpStep {
        match step {
            FlowStep::Ask(FlowAsk::OAuth(ask)) => HttpStep::Ask(HttpAsk::OAuth { ask }),
            FlowStep::Ask(FlowAsk::Http(request)) => HttpStep::Ask(HttpAsk::Http { request, delay_ms: None }),
            FlowStep::Ask(FlowAsk::Random { bytes }) => HttpStep::Ask(HttpAsk::Random { bytes }),
            FlowStep::Ask(FlowAsk::Secret { name, scope }) => HttpStep::Ask(HttpAsk::Secret { name, scope }),
            FlowStep::Done(Ok(payload)) => self.done(Ok(payload)),
            FlowStep::Done(Err(FlowFailure::Reason(reason))) => self.fail_oauth(reason),
            FlowStep::Done(Err(FlowFailure::Refused(refusal))) => self.refuse(refusal),
            FlowStep::Done(Err(FlowFailure::Transport(error))) => self.fail(error),
        }
    }

    pub(super) fn next_retry_delay(&mut self, outcome: RetryOutcome) -> Option<u64> {
        let (retry, attempts) = match self.phase {
            Phase::GrantReply => {
                let Some(HttpAuth::TokenExchange(exchange)) = &self.spec.auth else {
                    return None;
                };
                (exchange.grant_request.as_ref()?.retry.as_ref()?, &mut self.grant_retries)
            }
            Phase::Reply => (self.spec.retry.as_ref()?, &mut self.retries),
            _ => return None,
        };
        retry.next_delay(attempts, outcome)
    }

    pub(super) fn retry_step(&mut self, outcome: RetryOutcome) -> Option<HttpStep> {
        let grant = matches!(self.phase, Phase::GrantReply);
        let delay = self.next_retry_delay(outcome)?;
        Some(if grant { self.send_grant(Some(delay)) } else { self.send_request(Some(delay)) })
    }

    pub(super) fn on_transport_error(&mut self, err: TransportError) -> HttpStep {
        let outcome = match &err {
            TransportError::TimedOut => Some(RetryOutcome::Timeout),
            TransportError::Failed(_) => Some(RetryOutcome::Failed),
            TransportError::Refused(_) => None,
        };
        if let Some(step) = outcome.and_then(|outcome| self.retry_step(outcome)) {
            return step;
        }
        self.fail(err)
    }

    pub(super) fn on_beacon(&mut self, result: Result<BeaconOutcome, TransportError>) -> HttpStep {
        let (sent, via, status) = match result {
            Ok(BeaconOutcome::Unavailable { message }) => return self.done(Err(unavailable_payload(&message))),
            Ok(BeaconOutcome::Queued) => (true, "queue", None),
            Ok(BeaconOutcome::Fetched { status }) => ((200..300).contains(&status), "fetch", Some(Value::Number(f64::from(status)))),
            Err(TransportError::TimedOut | TransportError::Failed(_)) => (false, "fetch", None),
            Err(err @ TransportError::Refused(_)) => return self.fail(err),
        };
        let mut fields = vec![("sent", Value::Bool(sent)), ("via", Value::string(via.to_string()))];
        if let Some(status) = status {
            fields.push(("status", status));
        }
        self.done(Ok(object(fields)))
    }

    pub(super) fn on_reply(&mut self, resp: HttpResponse) -> HttpStep {
        if let Some(step) = self.retry_step(RetryOutcome::Status(resp.status)) {
            return step;
        }
        if resp.status == 401 && !self.refreshed && self.exchange().is_some_and(|ex| ex.on_unauthorized == OnUnauthorized::Refresh) {
            if let Some(slot) = self.cache_slot() {
                self.refreshed = true;
                self.secrets.forget(&slot);
                return self.begin_grant();
            }
        }
        let payload = reply_payload(&resp, self.spec.response_as);
        let Some(paginate) = self.spec.paginate.clone() else {
            return self.done(Ok(payload));
        };
        self.pages += 1;
        if !resp.is_success() {
            return self.finish_pages(payload);
        }
        let mut ctx = BindingContext::new();
        ctx.set_local("response", payload.clone());
        let evaluator = Evaluator::new();
        let collected = match evaluator.evaluate(&SExpression::from(paginate.collect.clone()), &ctx) {
            Ok(Value::Array(items)) => items,
            Ok(_) => return self.fail(TransportError::Failed("paginate.collect did not produce a list".to_string())),
            Err(e) => return self.fail(TransportError::Failed(format!("paginate.collect: {e}"))),
        };
        self.items.extend(collected);
        if self.pages >= paginate.max {
            return self.finish_pages(payload);
        }
        let token = if paginate.style == PageStyle::LinkHeader {
            next_link(&resp)
        } else {
            (match evaluator.evaluate(&SExpression::from(paginate.next.clone()), &ctx) {
                Ok(Value::Null) => None,
                Ok(Value::String(s)) => Some(s),
                Ok(Value::Number(n)) => Some(js_number_string(n)).map(Into::into),
                Ok(_) => {
                    return self.fail(TransportError::Failed(
                        "paginate.next did not produce a string, number or null".to_string(),
                    ))
                }
                Err(e) => return self.fail(TransportError::Failed(format!("paginate.next: {e}"))),
            }).map(|v| v.into_inner())
        };
        match token {
            None => self.finish_pages(payload),
            Some(token) => {
                self.page_token = Some(token);
                self.retries = 0;
                self.pending_delay_ms = None;
                self.secrets.resource_request = None;
                self.send_page()
            }
        }
    }

    pub(super) fn finish_pages(&mut self, last: Value) -> HttpStep {
        let paged = object(vec![
            ("pages", Value::Number(f64::from(self.pages))),
            ("items", Value::array(std::mem::take(&mut self.items))),
            ("last", last),
        ]);
        self.done(Ok(paged))
    }
}
