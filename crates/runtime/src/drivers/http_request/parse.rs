//! Parsing an `http` call: egress checks, bodies, paging, beacons and credential slots.

use super::*;

/// Exact scheme + host + port match against an `egress` entry.
pub fn origin_allowed(url_or_origin: &str, egress: &[String]) -> bool {
    let Some((target, _)) = split_url(url_or_origin) else {
        return false;
    };
    egress
        .iter()
        .filter_map(|entry| parse_egress_entry(entry))
        .any(|allowed| allowed == target)
}

/// A TCP protocol driver's connect target (`redis://host[:port]`, `rediss://`, `postgres://`, `postgresql://`)
/// against `egress`: exact scheme, host and port, the default port of the scheme when none is written. Refuses
/// as `http` does, with [`HttpRefusal::EgressDenied`]; an `http(s)` entry never authorizes a TCP target.
pub fn tcp_target_allowed(origin: &str, egress: &[String]) -> Result<OriginParts, HttpRefusal> {
    let denied = || HttpRefusal::EgressDenied(origin.to_string());
    let (target, rest) = split_tcp_url(origin).ok_or_else(denied)?;
    if !rest.is_empty() {
        return Err(denied());
    }
    if egress.iter().filter_map(|entry| parse_egress_entry(entry)).any(|allowed| allowed == target) {
        Ok(target)
    } else {
        Err(denied())
    }
}

pub(super) fn scalar_text(value: &Value, what: &str) -> Result<Option<String>, HttpRefusal> {
    match value {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        Value::Number(n) => Ok(Some(js_number_string(*n))),
        Value::Bool(b) => Ok(Some(b.to_string())),
        _ => invalid(format!("{what} must be a string, number, boolean, null or a list of them")),
    }
}

pub(super) fn pairs_of(value: &Value, what: &str) -> Result<Vec<(String, String)>, HttpRefusal> {
    let Value::Object(map) = value else {
        return invalid(format!("{what} must be an object"));
    };
    let mut out = Vec::new();
    for (key, entry) in map {
        match entry {
            Value::Array(items) => {
                for item in items {
                    if let Some(text) = scalar_text(item, what)? {
                        out.push((key.clone(), text));
                    }
                }
            }
            other => {
                if let Some(text) = scalar_text(other, what)? {
                    out.push((key.clone(), text));
                }
            }
        }
    }
    Ok(out)
}

pub(super) fn slot_field(map: &ValueMap, key: &str, what: &str) -> Result<CredentialSlot, HttpRefusal> {
    let at = format!("{what}.{key}");
    match map.get(key) {
        Some(Value::String(name)) => Ok(name.as_str().into()),
        Some(Value::Object(slot)) => parse_slot_object(slot, &at),
        _ => invalid(format!("{at} must be a credential name or {{ credential, scope }}")),
    }
}

pub(super) fn non_empty_text(map: &ValueMap, key: &str, what: &str) -> Result<Option<String>, HttpRefusal> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(_) => invalid(format!("{what}.{key} must be a non-empty string")),
    }
}


pub(super) fn parse_webpush(value: &Value, target: &str) -> Result<HttpBodySpec, HttpRefusal> {
    let Value::Object(map) = value else { return invalid("webpush must be an object") };
    closed_keys(map, &["subscription", "payload", "ttl", "urgency"], "webpush")?;
    let Some(Value::Object(subscription)) = map.get("subscription") else { return invalid("webpush.subscription must be an object") };
    closed_keys(subscription, &["endpoint", "keys"], "webpush.subscription")?;
    let endpoint = text_field(subscription, "endpoint", "webpush.subscription")?;
    if endpoint != target { return invalid("webpush subscription.endpoint must equal the declared HTTP URL") }
    let Some(Value::Object(keys)) = subscription.get("keys") else { return invalid("webpush.subscription.keys must be an object") };
    closed_keys(keys, &["p256dh", "auth"], "webpush.subscription.keys")?;
    let urgency = map.get("urgency").map(|_| text_field(map,"urgency","webpush")).transpose()?.unwrap_or_else(|| "normal".into());
    if !["very-low","low","normal","high"].contains(&urgency.as_str()) { return invalid("invalid webpush.urgency") }
    Ok(HttpBodySpec::WebPush(WebPushBody {
        endpoint, p256dh:text_field(keys,"p256dh","webpush.subscription.keys")?,
        auth:text_field(keys,"auth","webpush.subscription.keys")?,
        payload:text_field(map,"payload","webpush")?,
        ttl:whole_number(map.get("ttl"),"webpush.ttl")?.unwrap_or(2_419_200), urgency,
    }))
}

pub(super) fn parse_multipart(value: &Value) -> Result<HttpBodySpec, HttpRefusal> {
    let Value::Object(map) = value else {
        return invalid("multipart must be an object");
    };
    closed_keys(map, &["kind", "parts"], "multipart")?;
    let kind = match text_field(map, "kind", "multipart")?.as_str() {
        "related" => MultipartKind::Related,
        "form-data" => MultipartKind::FormData,
        other => return invalid(format!("multipart.kind must be related or form-data, not `{other}`")),
    };
    let Some(Value::Array(raw)) = map.get("parts") else {
        return invalid("multipart.parts must be a list");
    };
    if raw.is_empty() {
        return invalid("multipart.parts needs at least one part");
    }
    let mut parts = Vec::new();
    for raw_part in raw {
        let Value::Object(part) = raw_part else {
            return invalid("a multipart part must be an object");
        };
        closed_keys(part, &["headers", "text", "bytes"], "multipart part")?;
        let mut headers = Vec::new();
        match part.get("headers") {
            None => {}
            Some(Value::Object(map)) => {
                for (name, value) in map {
                    match value {
                        Value::String(v) if is_header_name(name) && is_header_value(v) => {
                            headers.push((name.clone(), v.clone()))
                        }
                        _ => return invalid("multipart part header needs a token name and a single-line string value"),
                    }
                }
            }
            Some(_) => return invalid("multipart part headers must be an object"),
        }
        let content = match (part.get("text"), part.get("bytes")) {
            (Some(Value::String(text)), None) => PartContent::Text(text.clone()),
            (None, Some(Value::String(b64))) => match codec::base64_decode(b64) {
                Ok(bytes) => PartContent::Bytes(bytes),
                Err(e) => return invalid(format!("multipart part bytes are not base64: {e}")),
            },
            _ => return invalid("a multipart part carries exactly one of text, bytes (a string)"),
        };
        parts.push(MultipartPart { headers, content });
    }
    Ok(HttpBodySpec::Multipart { kind, parts })
}

pub(super) fn parse_paginate(value: &Value) -> Result<HttpPaginate, HttpRefusal> {
    let Value::Object(map) = value else {
        return invalid("paginate must be an object");
    };
    closed_keys(map, &["style", "param", "next", "collect", "max"], "paginate")?;
    let style = match text_field(map, "style", "paginate")?.as_str() {
        "cursor" => PageStyle::Cursor,
        "offset" => PageStyle::Offset,
        "page" => PageStyle::Page,
        "link-header" => PageStyle::LinkHeader,
        other => return invalid(format!("unknown paginate style `{other}`")),
    };
    let param = match map.get("param") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return invalid("paginate.param must be a string"),
    };
    if style != PageStyle::LinkHeader && param.is_none() {
        return invalid("paginate.param is required unless the style is link-header");
    }
    let next = map.get("next").cloned().unwrap_or(Value::Null);
    if style != PageStyle::LinkHeader && next.is_null() {
        return invalid("paginate.next is required unless the style is link-header");
    }
    let Some(collect) = map.get("collect").cloned() else {
        return invalid("paginate.collect is required");
    };
    let max = match map.get("max") {
        Some(Value::Number(n)) if *n >= 1.0 && n.fract() == 0.0 && *n <= f64::from(u32::MAX) => *n as u32,
        _ => return invalid("paginate.max must be a whole number of at least 1"),
    };
    Ok(HttpPaginate {
        style,
        param,
        next,
        collect,
        max,
    })
}

pub(super) const OAUTH_ENDPOINT_STAND_IN: &str = "https://oauth-endpoint.invalid";

/// `(http METHOD oauth-endpoint WORD { auth: { scheme: oauth-client, … } })`: the URL is the client's metadata
/// endpoint named by WORD, so the call has no origin of its own and egress is checked on each built request.
pub(super) fn parse_oauth_endpoint_call(method: HttpMethod, word: &str, request: &Value) -> Result<HttpCallSpec, HttpRefusal> {
    let Some(kind) = OAuthEndpointKind::parse(word) else {
        return invalid(format!("oauth-endpoint must be followed by one of {}, not `{word}`", OAuthEndpointKind::words().join(", ")));
    };
    let mut spec = parse_http_call(method, OAUTH_ENDPOINT_STAND_IN, &format!("/{word}"), request)?;
    match &spec.auth {
        Some(HttpAuth::OAuthClient { op, .. }) if op.endpoint() == Some(kind) => {}
        Some(HttpAuth::OAuthClient { op, .. }) => {
            return invalid(format!("oauth-endpoint {word} does not serve op `{}`", op.name()))
        }
        _ => return invalid("oauth-endpoint needs an oauth-client auth"),
    }
    let expected = if kind == OAuthEndpointKind::Userinfo { HttpMethod::Get } else { HttpMethod::Post };
    if method != expected {
        return invalid(format!("oauth-endpoint {word} is called with {}", expected.as_str()));
    }
    spec.origin = OAuthEndpointKind::target().to_string();
    spec.path = word.to_string();
    Ok(spec)
}

pub fn parse_http_call(
    method: HttpMethod,
    origin: &str,
    path: &str,
    request: &Value,
) -> Result<HttpCallSpec, HttpRefusal> {
    if origin == OAuthEndpointKind::target() {
        return parse_oauth_endpoint_call(method, path, request);
    }
    let Some((origin_parts, _)) = parse_origin_prefix(origin) else {
        return invalid(format!("origin `{origin}` is not scheme://host[:port][/prefix]"));
    };
    let origin = origin.trim_end_matches('/');
    orbital_core_base::http_wire::check_http_path(path)?;
    match split_url(&format!("{origin}{path}")) {
        Some((joined, _)) if joined == origin_parts => {}
        _ => return Err(HttpRefusal::OriginEscape(path.to_string())),
    }

    let empty = ValueMap::default();
    let map = match request {
        Value::Null => &empty,
        Value::Object(map) => map,
        _ => return invalid("request must be an object"),
    };
    closed_keys(
        map,
        &[
            "query", "headers", "json", "form", "text", "bytes", "multipart", "auth", "timeoutMs", "delayMs", "responseAs",
            "paginate", "stream", "retry", "formEncoding", "queryEncoding", "presign", "webpush",
        ],
        "request",
    )?;
    let query_encoding = match map.get("queryEncoding") {
        None => QueryEncoding::Uri,
        Some(Value::String(s)) if s == "uri" => QueryEncoding::Uri,
        Some(Value::String(s)) if s == "rfc3986" => QueryEncoding::Rfc3986,
        Some(_) => return invalid("queryEncoding must be uri or rfc3986"),
    };

    let query = match map.get("query") {
        Some(value) => pairs_of(value, "query")?,
        None => Vec::new(),
    };
    let headers = parse_http_headers(map.get("headers"))?;

    let bodies: Vec<&str> = ["json", "form", "text", "bytes", "multipart", "webpush"]
        .into_iter()
        .filter(|k| map.contains_key(*k))
        .collect();
    if bodies.len() > 1 {
        return invalid("a request carries at most one of json, form, text, bytes, multipart, webpush");
    }
    if !bodies.is_empty() && method == HttpMethod::Get {
        return invalid("a GET request carries no body");
    }
    let body = match bodies.first().copied() {
        None => HttpBodySpec::Empty,
        Some("webpush") => parse_webpush(&map["webpush"], &with_query(format!("{origin}{path}"), &encode_query(&query, query_encoding)))?,
        Some("json") => match serde_json::to_string(&value_to_json(&map["json"])) {
            Ok(text) => HttpBodySpec::Json(text),
            Err(e) => return invalid(format!("json body: {e}")),
        },
        Some("form") => HttpBodySpec::Form(pairs_of(&map["form"], "form")?),
        Some("text") => match &map["text"] {
            Value::String(s) => HttpBodySpec::Text(s.clone()),
            _ => return invalid("text body must be a string"),
        },
        Some("bytes") => match &map["bytes"] {
            Value::String(s) => match codec::base64_decode(s) {
                Ok(bytes) => HttpBodySpec::Bytes(bytes),
                Err(e) => return invalid(format!("bytes body is not base64: {e}")),
            },
            _ => return invalid("bytes body must be a base64 string"),
        },
        Some(_) => {
            if headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
                return invalid("a multipart body sets its own Content-Type");
            }
            parse_multipart(&map["multipart"])?
        }
    };
    let form_encoding = match map.get("formEncoding") {
        None => FormEncoding::Html,
        Some(_) if !matches!(body, HttpBodySpec::Form(_)) => return invalid("formEncoding applies to a form body"),
        Some(Value::String(s)) if s == "html" => FormEncoding::Html,
        Some(Value::String(s)) if s == "rfc3986" => FormEncoding::Rfc3986,
        Some(_) => return invalid("formEncoding must be html or rfc3986"),
    };
    let retry = map.get("retry").map(parse_retry).transpose()?;

    let auth = map.get("auth").map(parse_auth).transpose()?;
    if let Some(HttpAuth::OAuthClient { op, .. }) = &auth {
        if let Some(extra) = map.keys().find(|key| key.as_str() != "auth") {
            return invalid(format!("an oauth-client request carries only `auth`, not `{extra}`"));
        }
        let endpoint_call = origin == OAUTH_ENDPOINT_STAND_IN;
        if !endpoint_call && (*op != OAuthOp::Discovery || method != HttpMethod::Get) {
            return invalid("an oauth-client op other than discovery is sent to `oauth-endpoint`; discovery is a GET");
        }
    }
    let timeout_ms = parse_timeout_ms(map.get("timeoutMs"))?;
    let delay_ms = parse_delay_ms(map.get("delayMs"))?;
    let response_as = match map.get("responseAs") {
        None => ResponseAs::Text,
        Some(Value::String(s)) if s == "text" => ResponseAs::Text,
        Some(Value::String(s)) if s == "bytes" => ResponseAs::Bytes,
        Some(_) => return invalid("responseAs must be text or bytes"),
    };
    let paginate = map.get("paginate").map(parse_paginate).transpose()?;
    let stream = match map.get("stream") {
        None => None,
        Some(Value::String(s)) if s == "sse" => Some(StreamFraming::Sse),
        Some(Value::String(s)) if s == "json-lines" => Some(StreamFraming::JsonLines),
        Some(_) => return invalid("stream must be sse or json-lines"),
    };
    if paginate.is_some() && stream.is_some() {
        return invalid("a request is paginated or streamed, not both");
    }
    if retry.is_some() && stream.is_some() {
        return invalid("a streamed request is not retried");
    }
    let presign = match map.get("presign") {
        None => None,
        Some(Value::Object(options)) => {
            closed_keys(options, &["expiresSec"], "presign")?;
            let seconds = whole_number(options.get("expiresSec"), "presign.expiresSec")?
                .filter(|n| (1..=604800).contains(n))
                .ok_or_else(|| HttpRefusal::Invalid("presign.expiresSec must be a whole number from 1 to 604800".into()))?;
            if !matches!(auth, Some(HttpAuth::AwsSigV4 { .. })) {
                return invalid("presign requires aws-sigv4 auth");
            }
            if paginate.is_some() || stream.is_some() || retry.is_some() || !matches!(body, HttpBodySpec::Empty) {
                return invalid("presign has no body, pagination, stream or retry");
            }
            Some(seconds)
        }
        Some(_) => return invalid("presign must be an object"),
    };

    if delay_ms.is_some() && (stream.is_some() || presign.is_some()) {
        return invalid("delayMs is not used with stream or presign");
    }

    Ok(HttpCallSpec {
        method,
        origin: origin.to_string(),
        path: path.to_string(),
        query,
        headers,
        body,
        auth,
        timeout_ms,
        delay_ms,
        response_as,
        paginate,
        stream,
        form_encoding,
        query_encoding,
        retry,
        presign,
        delivery: HttpDelivery::Fetch,
    })
}

/// A `browser/beacon` call: `url` must be an absolute http(s) URL without a fragment and `request` is exactly
/// `{ text }`. The result is an ordinary POST spec whose delivery is the beacon port, so egress and the request
/// itself come from the same builder as every other call.
pub fn parse_beacon_call(url: &str, request: &Value) -> Result<HttpCallSpec, HttpRefusal> {
    let Some((_, remainder)) = split_url(url) else {
        return invalid(format!("beacon URL `{url}` is not an absolute http(s) URL"));
    };
    if remainder.contains('#') {
        return invalid("a beacon URL must not carry a fragment");
    }
    let origin = &url[..url.len() - remainder.len()];
    let path = if remainder.is_empty() { "/".to_string() } else if remainder.starts_with('?') { format!("/{remainder}") } else { remainder.to_string() };
    let Value::Object(map) = request else {
        return invalid("a beacon request is an object carrying `text`");
    };
    closed_keys(map, &["text"], "beacon request")?;
    let Some(Value::String(text)) = map.get("text") else {
        return invalid("a beacon request needs `text`, a string");
    };
    let mut body = ValueMap::default();
    body.insert("text".to_string(), Value::String(text.clone()));
    body.insert(
        "headers".to_string(),
        Value::Object(ValueMap::from_iter([("Content-Type".to_string(), Value::String("text/plain".to_string()))])),
    );
    let mut spec = parse_http_call(HttpMethod::Post, origin, &path, &Value::Object(body))?;
    spec.delivery = HttpDelivery::Beacon;
    Ok(spec)
}

/// The host-secret name a token exchange keeps its token under.
pub fn token_cache_name(auth: &HttpAuth) -> Option<String> {
    match auth {
        HttpAuth::TokenExchange(ex) => Some(ex.store_as.name.clone()),
        _ => None,
    }
}

/// The slots the exchange itself needs, in ask order: the client id and secret, then the body's slots.
pub(super) fn exchange_slots(ex: &TokenExchange) -> Vec<CredentialSlot> {
    let mut out: Vec<CredentialSlot> = Vec::new();
    let body = ex.fields.iter().filter_map(|(_, field)| match field {
        TokenField::Slot(slot) | TokenField::JwtAssertion { key: slot, .. } => Some(slot),
        TokenField::Literal(_) => None,
    });
    for slot in ex.client_id.iter().chain(ex.client_secret.iter()).chain(body) {
        if !out.contains(slot) {
            out.push(slot.clone());
        }
    }
    out
}

/// Credential slots `build_request`'s lookup must answer, in ask order. For a token exchange that is the cached token.
pub fn credential_slots_needed(spec: &HttpCallSpec) -> Vec<CredentialSlot> {
    match &spec.auth {
        None => Vec::new(),
        Some(HttpAuth::Bearer { credential })
        | Some(HttpAuth::ApiKey { credential, .. })
        | Some(HttpAuth::HmacSha256 { credential, .. }) => vec![credential.clone()],
        Some(HttpAuth::Basic { user, credential }) => vec![user.clone(), credential.clone()],
        Some(HttpAuth::TokenExchange(ex)) => vec![ex.store_as.slot()],
        Some(HttpAuth::Vapid { private_key, .. }) => vec![private_key.clone()],
        Some(HttpAuth::OAuthClient { clients, .. }) if !clients.is_empty() => Vec::new(),
        Some(HttpAuth::OAuthClient { client, .. }) if client.mode == OAuthMode::Mock => Vec::new(),
        Some(HttpAuth::OAuthClient { client, .. }) => {
            std::iter::once(client.client_id.clone()).chain(client.client_secret.clone()).collect()
        }
        Some(HttpAuth::AwsSigV4 { access_key_id, secret_access_key, session_token, .. }) =>
            [Some(access_key_id.clone()), Some(secret_access_key.clone()), session_token.clone()].into_iter().flatten().collect(),
    }
}

/// Credential names `build_request`'s lookup must answer, in ask order.
pub fn credentials_needed(spec: &HttpCallSpec) -> Vec<String> {
    credential_slots_needed(spec).into_iter().map(|slot| slot.name).collect()
}
