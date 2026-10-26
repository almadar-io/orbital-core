//! Building the request, following page links, and the reply and failure payloads.

use super::*;

pub(super) fn encode_query(pairs: &[(String, String)], encoding: QueryEncoding) -> String {
    let escape = match encoding {
        QueryEncoding::Uri => codec::encode_uri_component,
        QueryEncoding::Rfc3986 => codec::encode_rfc3986,
    };
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", escape(k), escape(v)))
        .collect::<Vec<_>>()
        .join("&")
}

pub(super) fn with_query(url: String, query: &str) -> String {
    if query.is_empty() {
        url
    } else if url.contains('?') {
        format!("{url}&{query}")
    } else {
        format!("{url}?{query}")
    }
}

pub(super) fn resolve_page_url(origin: &str, token: &str) -> Result<String, HttpRefusal> {
    let Some((expected, prefix)) = parse_origin_prefix(origin) else {
        return invalid(format!("origin `{origin}` is not scheme://host[:port][/prefix]"));
    };
    if token.starts_with('/') && !token.starts_with("//") && !token.contains('\\') {
        return Ok(format!("{}{token}", &origin[..origin.len() - prefix.len()]));
    }
    match split_url(token) {
        Some((parts, _)) if parts == expected => Ok(token.to_string()),
        _ => Err(HttpRefusal::OriginEscape(token.to_string())),
    }
}

pub fn build_request(
    spec: &HttpCallSpec,
    secrets: &dyn Fn(&CredentialSlot) -> Option<String>,
    egress: &[String],
    page_token: Option<&str>,
) -> Result<HttpRequest, HttpRefusal> {
    build_request_with_boundary(spec, secrets, egress, page_token, None)
}

/// [`build_request`] with the multipart boundary the driver drew from the host's randomness.
pub fn build_request_with_boundary(
    spec: &HttpCallSpec,
    secrets: &dyn Fn(&CredentialSlot) -> Option<String>,
    egress: &[String],
    page_token: Option<&str>,
    boundary: Option<&str>,
) -> Result<HttpRequest, HttpRefusal> {
    build_request_with_metadata(spec, secrets, egress, page_token, boundary, None, None)
}

pub(super) fn build_request_with_metadata(
    spec: &HttpCallSpec,
    secrets: &dyn Fn(&CredentialSlot) -> Option<String>,
    egress: &[String],
    page_token: Option<&str>,
    boundary: Option<&str>,
    now_ms: Option<f64>,
    webpush_body: Option<&[u8]>,
) -> Result<HttpRequest, HttpRefusal> {
    if !origin_allowed(&spec.origin, egress) {
        return Err(HttpRefusal::EgressDenied(spec.origin.clone()));
    }

    let mut query = spec.query.clone();
    let mut url = None;
    if let (Some(token), Some(paginate)) = (page_token, &spec.paginate) {
        if paginate.style == PageStyle::LinkHeader {
            url = Some(resolve_page_url(&spec.origin, token)?);
            query.clear();
        } else if let Some(param) = &paginate.param {
            query.retain(|(k, _)| k != param);
            query.push((param.clone(), token.to_string()));
        }
    }
    let mut headers = spec.headers.clone();

    match &spec.auth {
        None => {}
        Some(HttpAuth::OAuthClient { .. }) => {}
        Some(HttpAuth::AwsSigV4 { .. }) => {}
        Some(HttpAuth::Vapid { .. }) => {}
        Some(HttpAuth::Bearer { credential: name }) => {
            headers.push((
                "Authorization".to_string(),
                format!("Bearer {}", credential(secrets, name)?),
            ));
        }
        Some(HttpAuth::ApiKey {
            credential: name,
            name: key,
            location,
        }) => {
            let value = credential(secrets, name)?;
            match location {
                ApiKeyIn::Header => headers.push((key.clone(), value)),
                ApiKeyIn::Query => query.push((key.clone(), value)),
            }
        }
        Some(HttpAuth::Basic { user, credential: name }) => {
            let pair = format!("{}:{}", credential(secrets, user)?, credential(secrets, name)?);
            headers.push((
                "Authorization".to_string(),
                format!("Basic {}", codec::base64_encode(pair.as_bytes())),
            ));
        }
        Some(HttpAuth::TokenExchange(ex)) => {
            headers.push((
                "Authorization".to_string(),
                format!("Bearer {}", credential(secrets, &ex.store_as.slot())?),
            ));
        }
        Some(HttpAuth::HmacSha256 {
            credential: name,
            name: header,
            signs,
            encoding,
            prefix,
        }) => {
            let mac = codec::hmac_sha256(credential(secrets, name)?.as_bytes(), signs.as_bytes());
            let value = match encoding {
                DigestEncoding::Hex => codec::hex(&mac),
                DigestEncoding::Base64 => codec::base64_encode(&mac),
            };
            headers.push((header.clone(), format!("{}{value}", prefix.as_deref().unwrap_or_default())));
        }
    }

    let url = match url {
        Some(url) => url,
        None => format!("{}{}", spec.origin, spec.path),
    };
    let mut url = with_query(url, &encode_query(&query, spec.query_encoding));
    match (split_url(&url), parse_origin_prefix(&spec.origin)) {
        (Some((joined, _)), Some((expected, _))) if joined == expected => {}
        _ => return Err(HttpRefusal::OriginEscape(spec.path.clone())),
    }

    let (content_type, body) = match &spec.body {
        HttpBodySpec::Empty => (None, Vec::new()),
        HttpBodySpec::Json(text) => (Some("application/json".to_string()), text.clone().into_bytes()),
        HttpBodySpec::Form(pairs) => (
            Some("application/x-www-form-urlencoded".to_string()),
            match spec.form_encoding {
                FormEncoding::Html => codec::form_urlencode(pairs),
                FormEncoding::Rfc3986 => pairs
                    .iter()
                    .map(|(k, v)| format!("{}={}", codec::encode_rfc3986(k), codec::encode_rfc3986(v)))
                    .collect::<Vec<_>>()
                    .join("&"),
            }
            .into_bytes(),
        ),
        HttpBodySpec::Text(text) => (Some("text/plain; charset=utf-8".to_string()), text.clone().into_bytes()),
        HttpBodySpec::Bytes(bytes) => (Some("application/octet-stream".to_string()), bytes.clone()),
        HttpBodySpec::WebPush(push) => {
            if url != push.endpoint { return invalid("webpush subscription.endpoint must equal the declared HTTP URL") }
            for (name, _) in &headers {
                if ["content-type","content-encoding","content-length","ttl","urgency"].iter().any(|owned| name.eq_ignore_ascii_case(owned)) {
                    return invalid("webpush encryption headers are kernel-owned");
                }
            }
            let bytes = webpush_body.ok_or_else(|| HttpRefusal::Invalid("webpush needs journaled driver entropy".into()))?;
            headers.extend([
                ("TTL".into(),push.ttl.to_string()), ("Urgency".into(),push.urgency.clone()),
                ("Content-Length".into(),bytes.len().to_string()), ("Content-Encoding".into(),"aes128gcm".into()),
            ]);
            (Some("application/octet-stream".into()),bytes.to_vec())
        }
        HttpBodySpec::Multipart { kind, parts } => {
            let Some(boundary) = boundary else {
                return invalid("a multipart body needs a boundary from the driver");
            };
            (
                Some(format!("multipart/{}; boundary={boundary}", kind.as_str())),
                multipart_body(parts, boundary),
            )
        }
    };
    if let Some(content_type) = content_type {
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
            headers.push(("Content-Type".to_string(), content_type));
        }
    }

    if let Some(HttpAuth::AwsSigV4 { access_key_id, secret_access_key, session_token, region, service }) = &spec.auth {
        let stamp = dispatch_stamp(now_ms, "aws-sigv4")?;
        for (name, _) in &headers {
            if ["host", "authorization", "x-amz-date", "x-amz-content-sha256", "x-amz-security-token"].iter().any(|reserved| name.eq_ignore_ascii_case(reserved)) {
                return invalid("aws-sigv4 signing headers are kernel-owned");
            }
        }
        let signer = crate::sigv4::Signer {
            access_key_id: credential(secrets, access_key_id)?,
            secret_access_key: credential(secrets, secret_access_key)?,
            session_token: session_token.as_ref().map(|slot| credential(secrets, slot)).transpose()?,
            region: region.clone(), service: service.clone(),
        };
        let parsed = url::Url::parse(&url).map_err(|_| HttpRefusal::Invalid("invalid signing URL".into()))?;
        let host = &parsed[url::Position::BeforeHost..url::Position::AfterPort];
        let query: Vec<(String, String)> = parsed.query_pairs().into_owned().collect();
        let target = crate::sigv4::Target { method: spec.method.as_str(), host, path: parsed.path(), query: &query };
        if let Some(seconds) = spec.presign {
            if !headers.is_empty() { return invalid("presign cannot require headers not carried in its URL"); }
            let signed_query = signer.presign_query(&target, &seconds.to_string(), Some(crate::sigv4::UNSIGNED_PAYLOAD), stamp);
            url = format!("{}?{signed_query}", &parsed[..url::Position::AfterPath]);
        } else {
            headers.extend(signer.sign_request(&target, &headers, &body, stamp));
        }
    }
    if let Some(HttpAuth::Vapid { private_key, public_key, subject }) = &spec.auth {
        if !url.starts_with("https://android.googleapis.com/gcm/send") {
            if headers.iter().any(|(name,_)| name.eq_ignore_ascii_case("authorization")) { return invalid("vapid authorization is kernel-owned") }
            let stamp = dispatch_stamp(now_ms,"vapid")?;
            let (_,authority) = url.split_once("://").ok_or_else(|| HttpRefusal::Invalid("invalid VAPID URL".into()))?;
            let host = authority.split(['/', '?', '#']).next().unwrap_or("");
            let scheme = url.split_once("://").map(|(scheme,_)| scheme).unwrap_or("");
            let audience = format!("{}://{}",scheme.to_ascii_lowercase(),host.to_ascii_lowercase());
            let jwt = crate::webpush::vapid_jwt(&audience,subject,public_key,&credential(secrets,private_key)?,stamp.div_euclid(1000)+43_200)
                .map_err(|error| HttpRefusal::Invalid(error.0))?;
            headers.push(("Authorization".into(),format!("vapid t={jwt}, k={public_key}")));
        }
    }
    for (name, value) in &headers {
        check_header(name, value)?;
    }

    Ok(HttpRequest {
        method: spec.method.as_str().to_string(),
        url,
        headers,
        body,
        timeout: spec.timeout_ms.map(Duration::from_millis),
    })
}

pub(super) fn dispatch_stamp(now_ms: Option<f64>, scheme: &str) -> Result<i64, HttpRefusal> {
    now_ms.filter(|n| n.is_finite() && n.fract() == 0.0 && n.abs() <= 8_640_000_000_000_000.0)
        .map(|n| n as i64)
        .ok_or_else(|| HttpRefusal::Invalid(format!("{scheme} requires the dispatch nowMs stamp")))
}

pub(super) fn multipart_body(parts: &[MultipartPart], boundary: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        for (name, value) in &part.headers {
            out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        match &part.content {
            PartContent::Text(text) => out.extend_from_slice(text.as_bytes()),
            PartContent::Bytes(bytes) => out.extend_from_slice(bytes),
        }
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

/// The request that exchanges credentials for a token (`oauth2-client` is its client-credentials form).
pub fn token_exchange_request(
    auth: &HttpAuth,
    secrets: &dyn Fn(&CredentialSlot) -> Option<String>,
) -> Result<HttpRequest, HttpRefusal> {
    let HttpAuth::TokenExchange(ex) = auth else {
        return invalid("not a token-exchange auth");
    };
    if split_url(&ex.token_url).is_none() {
        return invalid(format!("tokenUrl `{}` is not an http(s) URL", ex.token_url));
    }
    let id = ex.client_id.as_ref().map(|slot| credential(secrets, slot)).transpose()?;
    let secret = ex.client_secret.as_ref().map(|slot| credential(secrets, slot)).transpose()?;
    let mut fields: Vec<(String, Value)> = Vec::new();
    if let Some(grant_type) = ex.grant.grant_type() {
        fields.push(("grant_type".to_string(), Value::string(grant_type.to_string())));
    }
    for (name, field) in &ex.fields {
        let value = match field {
            TokenField::Literal(value) => value.clone(),
            TokenField::Slot(slot) => Value::string(credential(secrets, slot)?),
            TokenField::JwtAssertion { key, alg, claims } => {
                let raw = credential(secrets, key)?;
                let parsed = crate::jwt::ServiceAccountKey::parse(&raw).or_else(|_| {
                    let bytes = crate::lenient::base64_decode_lenient(&raw);
                    let text = std::str::from_utf8(&bytes).map_err(|_| "invalid credential".to_string())?;
                    crate::jwt::ServiceAccountKey::parse(text)
                }).map_err(|_| HttpRefusal::Invalid("invalid service-account credential".to_string()))?;
                let pem = parsed.private_key.filter(|pem| !pem.is_empty())
                    .ok_or_else(|| HttpRefusal::Invalid("No key or keyFile set.".to_string()))?;
                let issuer = parsed.client_email.filter(|email| !email.is_empty())
                    .ok_or_else(|| HttpRefusal::Invalid("email is required.".to_string()))?;
                let mut payload = serde_json::Map::new();
                payload.insert("iss".to_string(), serde_json::Value::String(issuer));
                let Value::Object(claims) = claims else {
                    return invalid("auth.claims must be an object");
                };
                for (name, value) in claims {
                    payload.insert(name.clone(), value_to_json(value));
                }
                let payload = serde_json::Value::Object(payload).to_string();
                let assertion = crate::jwt::compact(*alg, &payload, &pem)
                    .map_err(|_| HttpRefusal::Invalid("invalid service-account signing key".to_string()))?;
                Value::string(assertion)
            }
        };
        fields.push((name.clone(), value));
    }
    let mut headers = ex.grant_request.as_ref().map(|request| request.headers.clone()).unwrap_or_default();
    if let (Some(id), Some(secret)) = (id, secret) {
        match ex.client_auth {
            Some(ClientAuth::Basic) => headers.push((
                "Authorization".to_string(),
                format!("Basic {}", codec::base64_encode(format!("{id}:{secret}").as_bytes())),
            )),
            Some(ClientAuth::Post) => {
                fields.push(("client_id".to_string(), Value::string(id)));
                fields.push(("client_secret".to_string(), Value::string(secret)));
            }
            None => {}
        }
    }
    let (content_type, body) = match ex.body_kind {
        TokenBodyKind::Form => {
            let mut pairs = Vec::new();
            for (name, value) in &fields {
                if let Some(text) = scalar_text(value, name)? {
                    pairs.push((name.clone(), text));
                }
            }
            ("application/x-www-form-urlencoded", codec::form_urlencode(&pairs))
        }
        TokenBodyKind::Json => {
            let object: serde_json::Map<String, serde_json::Value> =
                fields.iter().map(|(name, value)| (name.clone(), value_to_json(value))).collect();
            match serde_json::to_string(&object) {
                Ok(text) => ("application/json", text),
                Err(e) => return invalid(format!("token exchange body: {e}")),
            }
        }
    };
    headers.push(("Content-Type".to_string(), content_type.to_string()));
    Ok(HttpRequest {
        method: "POST".to_string(),
        url: ex.token_url.clone(),
        headers,
        body: body.into_bytes(),
        timeout: ex.grant_request.as_ref().and_then(|request| request.timeout_ms).map(Duration::from_millis),
    })
}

/// `(token, ttl_ms)` from a token reply. The expiry is optional: without it the ttl is 0, which the host reads as
/// "do not cache" (the kernel has no clock to age an entry). The margin is taken off the lifetime; a lifetime
/// that does not outlast it is stored for 0 ms too.
pub fn parse_token_reply(
    resp: &HttpResponse,
    access_field: &str,
    expires_field: Option<&str>,
    margin_ms: u64,
) -> Result<(String, u64), String> {
    if !resp.is_success() {
        return Err(format!("token request failed with status {}", resp.status));
    }
    let json: serde_json::Value =
        serde_json::from_slice(&resp.body).map_err(|e| format!("token reply is not JSON: {e}"))?;
    let token = json
        .get(access_field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("token reply has no {access_field}"))?;
    let ttl_ms = match expires_field.and_then(|field| json.get(field)) {
        None | Some(serde_json::Value::Null) => 0,
        Some(value) => {
            let seconds = match value {
                serde_json::Value::String(text) => text.parse::<f64>().ok(),
                other => other.as_f64(),
            }
            .filter(|n| n.is_finite() && *n >= 0.0)
            .ok_or_else(|| format!("token reply has an invalid {}", expires_field.unwrap_or_default()))?;
            ((seconds * 1000.0) as u64).saturating_sub(margin_ms)
        }
    };
    Ok((token.to_string(), ttl_ms))
}

/// The replacement refresh token a rotating grant reply carries in `field`.
pub(super) fn parse_rotated_token(resp: &HttpResponse, field: &str) -> Result<String, String> {
    let json: serde_json::Value =
        serde_json::from_slice(&resp.body).map_err(|e| format!("token reply is not JSON: {e}"))?;
    json.get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("token reply has no {field}"))
}

pub(super) fn object(entries: Vec<(&str, Value)>) -> Value {
    Value::Object(entries.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

pub fn reply_payload(resp: &HttpResponse, as_: ResponseAs) -> Value {
    let mut headers = ValueMap::default();
    for (name, value) in &resp.headers {
        let name = name.to_ascii_lowercase();
        let joined = match headers.get(&name) {
            Some(Value::String(prev)) => format!("{prev}, {value}"),
            _ => value.clone(),
        };
        headers.insert(name, Value::string(joined));
    }
    let body = match as_ {
        ResponseAs::Text => resp.text().into_owned(),
        ResponseAs::Bytes => codec::base64_encode(&resp.body),
    };
    object(vec![
        ("status", Value::Number(f64::from(resp.status))),
        ("headers", Value::object(headers)),
        ("body", Value::string(body)),
    ])
}

pub(super) fn failure_value(error: String, kind: &str, credential: Option<String>) -> Value {
    object(vec![
        ("error", Value::string(error)),
        ("kind", Value::string(kind.to_string())),
        ("credential", credential.map_or(Value::Null, Value::string)),
        ("phase", Value::Null),
        ("grantStatus", Value::Null),
        ("grantBody", Value::Null),
        ("code", Value::Null),
        ("reason", Value::Null),
    ])
}

pub(super) fn set_field(mut payload: Value, name: &str, value: Value) -> Value {
    if let Value::Object(map) = &mut payload {
        map.insert(name.to_string(), value);
    }
    payload
}

/// A failure of the OAuth flow: the fixed safe message, its public `code` and the closed `reason`.
pub fn oauth_failure_payload(error: &str, code: &str, reason: &str) -> Value {
    let payload = failure_value(error.to_string(), "oauth", None);
    let payload = set_field(payload, "code", Value::string(code.to_string()));
    set_field(payload, "reason", Value::string(reason.to_string()))
}

/// The host has no port for a capability the call needs.
pub fn unavailable_payload(message: &str) -> Value {
    failure_value(message.to_string(), "unavailable", None)
}

pub fn failure_payload(err: &TransportError) -> Value {
    match err {
        TransportError::TimedOut => failure_value(TransportError::TimedOut.to_string(), "timeout", None),
        TransportError::Failed(message) => failure_value(message.clone(), "failed", None),
        TransportError::Refused(message) => failure_value(message.clone(), "refused", None),
    }
}

/// A call refused before it was sent. A missing credential is `unconfigured` and names it; the rest are `refused`.
pub fn refusal_payload(r: &HttpRefusal) -> Value {
    match r {
        HttpRefusal::CredentialMissing(name) => failure_value(r.message(), "unconfigured", Some(name.clone())),
        _ => failure_value(r.message(), "refused", None),
    }
}

/// The URL of the `rel="next"` entry across the `Link` headers.
pub fn next_link(resp: &HttpResponse) -> Option<String> {
    for (name, value) in &resp.headers {
        if !name.eq_ignore_ascii_case("link") {
            continue;
        }
        for entry in split_link_entries(value) {
            let Some((target, params)) = entry.trim().split_once('>') else {
                continue;
            };
            let Some(url) = target.trim().strip_prefix('<') else {
                continue;
            };
            let is_next = params.split(';').any(|param| {
                match param.trim().split_once('=') {
                    Some((key, rel)) if key.trim().eq_ignore_ascii_case("rel") => rel
                        .trim()
                        .trim_matches('"')
                        .split_whitespace()
                        .any(|r| r.eq_ignore_ascii_case("next")),
                    _ => false,
                }
            });
            if is_next {
                return Some(url.to_string());
            }
        }
    }
    None
}

pub(super) fn split_link_entries(value: &str) -> Vec<&str> {
    let mut entries = Vec::new();
    let mut depth = false;
    let mut quoted = false;
    let mut start = 0;
    for (i, c) in value.char_indices() {
        match c {
            '<' if !quoted => depth = true,
            '>' if !quoted => depth = false,
            '"' => quoted = !quoted,
            ',' if !depth && !quoted => {
                entries.push(&value[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    entries.push(&value[start..]);
    entries
}
