//! Parsing an `http` call's auth: token bodies, exchanges, OAuth2, JWT bearer and OAuth clients.

use super::*;

pub(super) fn parse_token_body(value: Option<&Value>) -> Result<(TokenBodyKind, Vec<(String, TokenField)>), HttpRefusal> {
    let Some(value) = value else {
        return Ok((TokenBodyKind::Form, Vec::new()));
    };
    let Value::Object(body) = value else {
        return invalid("auth.body must be { json: {...} } or { form: {...} }");
    };
    closed_keys(body, &["json", "form"], "auth.body")?;
    let (kind, fields) = match (body.get("json"), body.get("form")) {
        (Some(fields), None) => (TokenBodyKind::Json, fields),
        (None, Some(fields)) => (TokenBodyKind::Form, fields),
        _ => return invalid("auth.body carries exactly one of json, form"),
    };
    let Value::Object(fields) = fields else {
        return invalid("auth.body fields must be an object");
    };
    let mut out = Vec::new();
    for (name, field) in fields {
        let at = format!("auth.body.{name}");
        out.push((
            name.clone(),
            match field {
                Value::Object(slot) if slot.contains_key("credential") => {
                    TokenField::Slot(parse_slot_object(slot, &at)?)
                }
                Value::Null if kind == TokenBodyKind::Form => continue,
                Value::String(_) | Value::Number(_) | Value::Bool(_) => TokenField::Literal(field.clone()),
                other if kind == TokenBodyKind::Json => TokenField::Literal(other.clone()),
                _ => return invalid(format!("{at} must be a string, number, boolean or credential slot in a form body")),
            },
        ));
    }
    Ok((kind, out))
}

pub(super) fn parse_store_as(value: &Value) -> Result<StoreAs, HttpRefusal> {
    let Value::Object(map) = value else {
        return invalid("auth.storeAs must be an object");
    };
    closed_keys(map, &["name", "scope"], "auth.storeAs")?;
    let Some(name) = non_empty_text(map, "name", "auth.storeAs")? else {
        return invalid("auth.storeAs.name is required");
    };
    let scope = match map.get("scope") {
        None => SecretScope::App,
        Some(Value::String(s)) if s == "app" => SecretScope::App,
        Some(Value::String(s)) if s == "user" => SecretScope::User,
        Some(_) => return invalid("auth.storeAs.scope must be app or user"),
    };
    Ok(StoreAs { name, scope })
}

pub(super) fn parse_token_rotate(value: &Value, grant: TokenGrant) -> Result<TokenRotate, HttpRefusal> {
    let Value::Object(map) = value else {
        return invalid("auth.rotate must be { field, credential }");
    };
    closed_keys(map, &["field", "credential"], "auth.rotate")?;
    if grant != TokenGrant::RefreshToken {
        return invalid("auth.rotate needs auth.grant: refresh_token");
    }
    let Some(field) = non_empty_text(map, "field", "auth.rotate")? else {
        return invalid("auth.rotate.field is required");
    };
    let credential = slot_field(map, "credential", "auth.rotate")?;
    Ok(TokenRotate { field, credential })
}

pub(super) fn parse_token_exchange(map: &ValueMap) -> Result<TokenExchange, HttpRefusal> {
    closed_keys(
        map,
        &[
            "scheme", "tokenUrl", "grant", "body", "clientAuth", "clientId", "clientSecret", "accessField",
            "expiresField", "expiresMarginMs", "storeAs", "grantRequest", "rotate", "onUnauthorized",
        ],
        "auth",
    )?;
    let token_url = text_field(map, "tokenUrl", "auth")?;
    if split_url(&token_url).is_none() {
        return invalid(format!("tokenUrl `{token_url}` is not an http(s) URL"));
    }
    let grant = match map.get("grant") {
        None => TokenGrant::Custom,
        Some(Value::String(s)) => match s.as_str() {
            "client_credentials" => TokenGrant::ClientCredentials,
            "refresh_token" => TokenGrant::RefreshToken,
            "authorization_code" => TokenGrant::AuthorizationCode,
            "custom" => TokenGrant::Custom,
            other => return invalid(format!("auth.grant must be client_credentials, refresh_token, authorization_code or custom, not `{other}`")),
        },
        Some(_) => return invalid("auth.grant must be a string"),
    };
    let (body_kind, fields) = parse_token_body(map.get("body"))?;
    let has_field = |name: &str| fields.iter().any(|(n, _)| n == name);
    if grant.grant_type().is_some() && has_field("grant_type") {
        return invalid("auth.body.grant_type is set by auth.grant; use grant: custom to send your own");
    }
    match grant {
        TokenGrant::RefreshToken if !has_field("refresh_token") => {
            return invalid("a refresh_token grant needs auth.body.refresh_token")
        }
        TokenGrant::AuthorizationCode if !has_field("code") => {
            return invalid("an authorization_code grant needs auth.body.code")
        }
        _ => {}
    }
    let client_auth = match map.get("clientAuth") {
        None => None,
        Some(Value::String(s)) if s == "basic" => Some(ClientAuth::Basic),
        Some(Value::String(s)) if s == "post" => Some(ClientAuth::Post),
        Some(_) => return invalid("auth.clientAuth must be basic or post"),
    };
    let (client_id, client_secret) = if client_auth.is_some() {
        (
            Some(slot_field(map, "clientId", "auth")?),
            Some(slot_field(map, "clientSecret", "auth")?),
        )
    } else if map.contains_key("clientId") || map.contains_key("clientSecret") {
        return invalid("auth.clientId and auth.clientSecret need auth.clientAuth");
    } else {
        (None, None)
    };
    let access_field = non_empty_text(map, "accessField", "auth")?.unwrap_or_else(|| "access_token".to_string());
    let expires_field = match map.get("expiresField") {
        None => Some("expires_in".to_string()),
        Some(Value::Null) => None,
        Some(_) => non_empty_text(map, "expiresField", "auth")?,
    };
    let expires_margin_ms = whole_number(map.get("expiresMarginMs"), "auth.expiresMarginMs")?.unwrap_or(0);
    let mut exchange = TokenExchange {
        token_url,
        grant,
        body_kind,
        fields,
        client_auth,
        client_id,
        client_secret,
        access_field,
        expires_field,
        expires_margin_ms,
        store_as: StoreAs { name: String::new(), scope: SecretScope::App },
        grant_request: map.get("grantRequest").map(parse_grant_request).transpose()?,
        rotate: map.get("rotate").map(|value| parse_token_rotate(value, grant)).transpose()?,
        on_unauthorized: parse_on_unauthorized(map)?,
    };
    if let Some(rotate) = &exchange.rotate {
        let body_slot = exchange.fields.iter().find_map(|(name, field)| match field {
            TokenField::Slot(slot) if name == "refresh_token" => Some(slot),
            _ => None,
        });
        if body_slot != Some(&rotate.credential) {
            return invalid("auth.rotate.credential must be the credential slot of auth.body.refresh_token");
        }
    }
    exchange.store_as = match map.get("storeAs") {
        Some(value) => parse_store_as(value)?,
        None => default_exchange_cache(&exchange, "token-exchange")?,
    };
    check_exchange_cache_scope(&exchange)?;
    check_grant_method(&exchange, "token-exchange")?;
    Ok(exchange)
}

pub(super) fn check_grant_method(exchange: &TokenExchange, scheme: &str) -> Result<(), HttpRefusal> {
    if exchange.grant_request.as_ref().map(|request| request.method) != Some(GrantMethod::Get) {
        return Ok(());
    }
    let mut carried = Vec::new();
    if exchange.grant.grant_type().is_some() {
        carried.push("auth.grant");
    }
    if !exchange.fields.is_empty() {
        carried.push("a request body");
    }
    if exchange.client_auth.is_some() {
        carried.push("auth.clientAuth");
    }
    if carried.is_empty() {
        return Ok(());
    }
    invalid(format!(
        "auth.grantRequest.method GET has nowhere to carry {} (scheme `{scheme}`); use method POST",
        carried.join(", ")
    ))
}

pub(super) fn check_exchange_cache_scope(exchange: &TokenExchange) -> Result<(), HttpRefusal> {
    if exchange.store_as.scope == SecretScope::App && exchange_slots(exchange).iter().any(|slot| slot.scope == SecretScope::User) {
        return invalid("a token exchange using user credentials requires a user-scoped cache");
    }
    Ok(())
}

pub(super) fn default_exchange_cache(exchange: &TokenExchange, prefix: &str) -> Result<StoreAs, HttpRefusal> {
    let slots = exchange_slots(exchange);
    let scope = if slots.iter().any(|slot| slot.scope == SecretScope::User) { SecretScope::User } else { SecretScope::App };
    let mut names = Vec::new();
    for slot in &slots {
        if !names.contains(&slot.name.as_str()) { names.push(slot.name.as_str()); }
    }
    names.sort_unstable();
    let mut fields = std::collections::BTreeMap::new();
    for (name, field) in &exchange.fields {
        let identity = match field {
            TokenField::Literal(value) => {
                let literal = match exchange.body_kind {
                    TokenBodyKind::Form => scalar_text(value, name)?.map_or(serde_json::Value::Null, serde_json::Value::String),
                    TokenBodyKind::Json => value_to_json(value),
                };
                serde_json::json!({"literal":literal})
            }
            TokenField::Slot(slot) => serde_json::json!({"slot":slot}),
            TokenField::JwtAssertion { key, alg, claims } => serde_json::json!({"assertion":{"key":key,"alg":alg,"claims":value_to_json(claims)}}),
        };
        fields.insert(name, identity);
    }
    let identity = serde_json::json!({
        "tokenUrl":exchange.token_url,"grant":exchange.grant,"bodyKind":exchange.body_kind,"fields":fields,
        "clientAuth":exchange.client_auth,"clientId":exchange.client_id,"clientSecret":exchange.client_secret,
        "accessField":exchange.access_field,"expiresField":exchange.expires_field,"expiresMarginMs":exchange.expires_margin_ms,"scope":scope,
    });
    let bytes = serde_json::to_vec(&orbital_core_eval::util::canonical_json(&identity))
        .map_err(|_| HttpRefusal::Invalid("invalid token-exchange cache identity".into()))?;
    Ok(StoreAs { name:format!("{prefix}:{}|{}|{}",exchange.token_url,names.join(","),codec::hex(&codec::sha256(&bytes))), scope })
}

/// `oauth2-client` is the client-credentials form of the token exchange.
pub(super) fn parse_oauth2_client(map: &ValueMap) -> Result<TokenExchange, HttpRefusal> {
    closed_keys(map, &["scheme", "tokenUrl", "clientId", "clientSecret", "scope", "grantRequest", "onUnauthorized"], "auth")?;
    let token_url = text_field(map, "tokenUrl", "auth")?;
    if split_url(&token_url).is_none() {
        return invalid(format!("tokenUrl `{token_url}` is not an http(s) URL"));
    }
    let client_id = slot_field(map, "clientId", "auth")?;
    let fields = match map.get("scope") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(scope)) => vec![("scope".to_string(), TokenField::Literal(Value::String(scope.clone())))],
        Some(_) => return invalid("auth.scope must be a string"),
    };
    let mut exchange = TokenExchange {
        store_as: StoreAs {
            name: format!("oauth2:{token_url}|{}", client_id.name),
            scope: SecretScope::App,
        },
        token_url,
        grant: TokenGrant::ClientCredentials,
        body_kind: TokenBodyKind::Form,
        fields,
        client_auth: Some(ClientAuth::Basic),
        client_id: Some(client_id),
        client_secret: Some(slot_field(map, "clientSecret", "auth")?),
        access_field: "access_token".to_string(),
        expires_field: Some("expires_in".to_string()),
        expires_margin_ms: 0,
        grant_request: map.get("grantRequest").map(parse_grant_request).transpose()?,
        rotate: None,
        on_unauthorized: parse_on_unauthorized(map)?,
    };
    check_grant_method(&exchange, "oauth2-client")?;
    exchange.store_as = default_exchange_cache(&exchange, "oauth2")?;
    Ok(exchange)
}

pub(super) fn parse_jwt_bearer(map: &ValueMap) -> Result<TokenExchange, HttpRefusal> {
    closed_keys(map, &["scheme", "key", "alg", "claims", "tokenUrl", "grant", "scope", "expiresMarginMs", "storeAs", "grantRequest", "onUnauthorized"], "auth")?;
    if let Some(request) = map.get("grantRequest").map(parse_grant_request).transpose()? {
        if request.method == GrantMethod::Get {
            return invalid("auth.grantRequest.method GET has nowhere to carry the signed assertion (scheme `jwt-bearer`); use method POST");
        }
    }
    let key = slot_field(map, "key", "auth")?;
    let alg = orbital_core_base::jwt_alg::JwtAlg::parse(&text_field(map, "alg", "auth")?)
        .ok_or_else(|| HttpRefusal::Invalid("auth.alg must be RS256 or ES256".to_string()))?;
    let Some(Value::Object(claims)) = map.get("claims") else {
        return invalid("auth.claims must be an object");
    };
    if claims.contains_key("iss") {
        return invalid("auth.claims.iss comes from the service-account credential");
    }
    let token_url = text_field(map, "tokenUrl", "auth")?;
    let grant = non_empty_text(map, "grant", "auth")?
        .unwrap_or_else(|| "urn:ietf:params:oauth:grant-type:jwt-bearer".to_string());
    let mut form = ValueMap::default();
    form.insert("grant_type".to_string(), Value::string(grant));
    if let Some(scope) = non_empty_text(map, "scope", "auth")? {
        form.insert("scope".to_string(), Value::string(scope));
    }
    let mut body = ValueMap::default();
    body.insert("form".to_string(), Value::object(form));
    let mut exchange = ValueMap::default();
    exchange.insert("scheme".to_string(), Value::string("token-exchange"));
    exchange.insert("tokenUrl".to_string(), Value::string(token_url.clone()));
    exchange.insert("body".to_string(), Value::object(body));
    for name in ["expiresMarginMs", "storeAs", "grantRequest", "onUnauthorized"] {
        if let Some(value) = map.get(name) {
            exchange.insert(name.to_string(), value.clone());
        }
    }
    let mut ex = parse_token_exchange(&exchange)?;
    if !map.contains_key("storeAs") {
        let identity_claims = claims.iter()
            .filter(|(name, _)| !matches!(name.as_str(), "iat" | "exp"))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let mut identity = ex.clone();
        identity.fields.insert(1, ("assertion".into(), TokenField::JwtAssertion {
            key: key.clone(), alg, claims: Value::Object(identity_claims),
        }));
        ex.store_as = default_exchange_cache(&identity, "jwt-bearer")?;
    }
    ex.fields.insert(1, ("assertion".to_string(), TokenField::JwtAssertion {
        key, alg, claims: Value::Object(claims.clone()),
    }));
    check_exchange_cache_scope(&ex)?;
    Ok(ex)
}

pub(super) fn parse_auth(value: &Value) -> Result<HttpAuth, HttpRefusal> {
    let Value::Object(map) = value else {
        return invalid("auth must be an object");
    };
    let scheme = text_field(map, "scheme", "auth")?;
    match scheme.as_str() {
        "bearer" => {
            closed_keys(map, &["scheme", "credential"], "auth")?;
            Ok(HttpAuth::Bearer {
                credential: slot_field(map, "credential", "auth")?,
            })
        }
        "api-key" => {
            closed_keys(map, &["scheme", "credential", "name", "in"], "auth")?;
            let location = match text_field(map, "in", "auth")?.as_str() {
                "header" => ApiKeyIn::Header,
                "query" => ApiKeyIn::Query,
                other => return invalid(format!("auth.in must be header or query, not `{other}`")),
            };
            let name = text_field(map, "name", "auth")?;
            if location == ApiKeyIn::Header && !is_header_name(&name) {
                return invalid("invalid HTTP auth header name");
            }
            Ok(HttpAuth::ApiKey {
                credential: slot_field(map, "credential", "auth")?,
                name,
                location,
            })
        }
        "basic" => {
            closed_keys(map, &["scheme", "user", "credential"], "auth")?;
            Ok(HttpAuth::Basic {
                user: slot_field(map, "user", "auth")?,
                credential: slot_field(map, "credential", "auth")?,
            })
        }
        "oauth2-client" => Ok(HttpAuth::TokenExchange(parse_oauth2_client(map)?)),
        "token-exchange" => Ok(HttpAuth::TokenExchange(parse_token_exchange(map)?)),
        "jwt-bearer" => Ok(HttpAuth::TokenExchange(parse_jwt_bearer(map)?)),
        "aws-sigv4" => {
            closed_keys(map, &["scheme", "accessKeyId", "secretAccessKey", "sessionToken", "region", "service"], "auth")?;
            Ok(HttpAuth::AwsSigV4 {
                access_key_id: slot_field(map, "accessKeyId", "auth")?,
                secret_access_key: slot_field(map, "secretAccessKey", "auth")?,
                session_token: map.get("sessionToken").filter(|v| !v.is_null()).map(|_| slot_field(map, "sessionToken", "auth")).transpose()?,
                region: non_empty_text(map, "region", "auth")?.ok_or_else(|| HttpRefusal::Invalid("auth.region is required".into()))?,
                service: non_empty_text(map, "service", "auth")?.ok_or_else(|| HttpRefusal::Invalid("auth.service is required".into()))?,
            })
        }
        "vapid" => {
            closed_keys(map, &["scheme", "privateKey", "publicKey", "subject"], "auth")?;
            Ok(HttpAuth::Vapid {
                private_key: slot_field(map, "privateKey", "auth")?,
                public_key: text_field(map, "publicKey", "auth")?,
                subject: text_field(map, "subject", "auth")?,
            })
        }
        "hmac-sha256" => {
            closed_keys(
                map,
                &["scheme", "credential", "name", "signs", "encoding", "prefix"],
                "auth",
            )?;
            let encoding = match text_field(map, "encoding", "auth")?.as_str() {
                "hex" => DigestEncoding::Hex,
                "base64" => DigestEncoding::Base64,
                other => return invalid(format!("auth.encoding must be hex or base64, not `{other}`")),
            };
            let name = text_field(map, "name", "auth")?;
            if !is_header_name(&name) {
                return invalid("invalid HTTP auth header name");
            }
            Ok(HttpAuth::HmacSha256 {
                credential: slot_field(map, "credential", "auth")?,
                name,
                signs: text_field(map, "signs", "auth")?,
                encoding,
                prefix: map.get("prefix").filter(|v| !v.is_null()).map(|_| text_field(map, "prefix", "auth")).transpose()?,
            })
        }
        "oauth-client" => parse_oauth_client_auth(map),
        other => invalid(format!("unknown auth scheme `{other}`")),
    }
}

/// An `oauth-client` client object: `{ tag, mode, issuer XOR endpoints, clientId, clientSecret?, clientAuth }`.
pub fn parse_oauth_client(value: &Value) -> Result<OAuthClientDecl, HttpRefusal> {
    let Value::Object(map) = value else {
        return invalid("auth.client must be an object");
    };
    closed_keys(map, &OAuthClientDecl::keys(), "auth.client")?;
    let tag = text_field(map, "tag", "auth.client")?;
    let tag = OAuthProvider::parse(&tag).ok_or_else(|| HttpRefusal::Invalid(format!("auth.client.tag must be google, github or auth0, not `{tag}`")))?;
    let mode = match text_field(map, "mode", "auth.client")?.as_str() {
        "real" => OAuthMode::Real,
        "mock" => OAuthMode::Mock,
        other => return invalid(format!("auth.client.mode must be real or mock, not `{other}`")),
    };
    let client_auth = match text_field(map, "clientAuth", "auth.client")?.as_str() {
        "basic" => orbital_core_base::oauth_wire::ClientAuth::Basic,
        "post" => orbital_core_base::oauth_wire::ClientAuth::Post,
        "none" => orbital_core_base::oauth_wire::ClientAuth::None,
        other => return invalid(format!("auth.client.clientAuth must be basic, post or none, not `{other}`")),
    };
    let server = match (map.get("issuer"), map.get("endpoints")) {
        (Some(Value::String(issuer)), None) if !issuer.is_empty() => OAuthServerDecl::Issuer(issuer.clone().into_inner()),
        (None, Some(Value::Object(endpoints))) => {
            closed_keys(endpoints, &["authorization", "token", "revocation", "userinfo"], "auth.client.endpoints")?;
            let optional = |key: &str| -> Result<Option<String>, HttpRefusal> {
                if endpoints.contains_key(key) { text_field(endpoints, key, "auth.client.endpoints").map(Some) } else { Ok(None) }
            };
            OAuthServerDecl::Endpoints {
                authorization: text_field(endpoints, "authorization", "auth.client.endpoints")?,
                token: text_field(endpoints, "token", "auth.client.endpoints")?,
                revocation: optional("revocation")?,
                userinfo: optional("userinfo")?,
            }
        }
        _ => return invalid("auth.client needs exactly one of issuer (a URL) or endpoints (an object)"),
    };
    Ok(OAuthClientDecl {
        tag,
        mode,
        server,
        client_id: slot_field(map, "clientId", "auth.client")?,
        client_secret: match client_auth {
            // A public client has no secret: a declared slot is never read, so a host that holds none is not "unconfigured".
            orbital_core_base::oauth_wire::ClientAuth::None => None,
            _ => map.get("clientSecret").map(|_| slot_field(map, "clientSecret", "auth.client")).transpose()?,
        },
        client_auth,
    })
}

pub(super) fn parse_oauth_client_auth(map: &ValueMap) -> Result<HttpAuth, HttpRefusal> {
    let op_word = text_field(map, "op", "auth")?;
    let op = OAuthOp::parse(&op_word).ok_or_else(|| HttpRefusal::Invalid(format!("auth.op must be one of {}, not `{op_word}`", OAuthOp::words().join(", "))))?;
    let (required, optional) = op.inputs();
    let mut allowed: Vec<&str> = vec!["scheme", "op", "client"];
    if op == OAuthOp::Code {
        allowed.push("clients");
    }
    allowed.extend(required.iter().copied());
    allowed.extend(optional.iter().copied());
    closed_keys(map, &allowed, "auth")?;
    let (client, clients) = match (map.get("client"), map.get("clients")) {
        (Some(client), None) => (parse_oauth_client(client)?, Vec::new()),
        (None, Some(Value::Object(declared))) => {
            let mut clients = Vec::new();
            for (tag, entry) in declared {
                let client = parse_oauth_client(entry)?;
                if client.tag.name() != tag {
                    return invalid(format!("auth.clients.{tag} declares a client tagged `{}`", client.tag.name()));
                }
                clients.push(client);
            }
            let Some(first) = clients.first().cloned() else {
                return invalid("auth.clients declares no client");
            };
            (first, clients)
        }
        (None, Some(_)) => return invalid("auth.clients must be an object of clients keyed by provider tag"),
        (Some(_), Some(_)) => return invalid("an oauth-client auth has `client` or `clients`, not both"),
        (None, None) => return invalid("auth.client is required"),
    };
    for key in required {
        if !map.contains_key(key) {
            return invalid(format!("auth op `{}` needs `{key}`", op.name()));
        }
    }
    let input = |key: &str| -> Result<Option<String>, HttpRefusal> {
        if map.contains_key(key) { text_field(map, key, "auth").map(Some) } else { Ok(None) }
    };
    let provider = match input("provider")? {
        Some(word) => Some(OAuthProvider::parse(&word).ok_or_else(|| HttpRefusal::Invalid(format!("auth.provider must be google, github or auth0, not `{word}`")))?),
        None => None,
    };
    let inputs = OAuthInputs {
        code: input("code")?,
        state: input("state")?,
        subject: input("subject")?,
        provider,
        refresh_token: input("refreshToken")?,
        token: input("token")?,
        access_token: input("accessToken")?,
    };
    Ok(HttpAuth::OAuthClient { op, client, clients, inputs })
}
