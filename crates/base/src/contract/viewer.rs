//! The viewer: who `@user` is, as one typed record. `@almadar/core`'s `Viewer` is generated from this type, and a
//! provider's claims are a typed list rather than open keys. The `@user` binding holds the same viewer flat (claims as
//! keys); [`Viewer::from_user`] and [`Viewer::to_user`] convert between the two.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::{Deserialize, Serialize};

use crate::value::{Value, ValueMap};

/// One provider or identity claim beyond the named viewer fields.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub name: String,
    /// The claim as JSON, exactly as the token carried it (a date as its ISO text).
    #[cfg_attr(feature = "ts-export", ts(type = "JsonValue"))]
    pub value: serde_json::Value,
}

/// Who `@user` is.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Viewer {
    /// Stable subject identifier: the ownership key.
    pub id: String,
    /// A token may carry no email or name, or carry it as null (owner ruling 2026-10-24).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional = nullable))]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional = nullable))]
    pub name: Option<String>,
    /// Compared against a policy's allowed roles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<String>>,
    #[serde(default)]
    pub claims: Vec<Claim>,
}

fn texts_of(items: &[Value], at: &str) -> Result<Vec<String>, String> {
    items
        .iter()
        .map(|item| match item {
            Value::String(text) => Ok(text.clone()),
            _ => Err(format!("viewer claim {at}: a list claim holds only text")),
        })
        .collect()
}

fn json_of(value: &Value, at: &str) -> Result<serde_json::Value, String> {
    Ok(match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(flag) => serde_json::Value::Bool(*flag),
        Value::String(text) => serde_json::Value::String(text.clone()),
        Value::Number(number) if number.fract() == 0.0 && number.abs() < 9_007_199_254_740_992.0 => {
            serde_json::Value::from(*number as i64)
        }
        Value::Number(number) => serde_json::Number::from_f64(*number)
            .map(serde_json::Value::Number)
            .ok_or_else(|| format!("viewer claim {at}: {number} is not a JSON number"))?,
        Value::Array(items) => serde_json::Value::Array(
            items.iter().map(|item| json_of(item, at)).collect::<Result<_, _>>()?,
        ),
        Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(key, item)| Ok((key.clone(), json_of(item, &format!("{at}.{key}"))?)))
                .collect::<Result<_, String>>()?,
        ),
        Value::Lambda { .. } => return Err(format!("viewer claim {at}: a function is not a claim")),
    })
}

const NAMED: [&str; 5] = ["id", "email", "name", "role", "permissions"];

impl Viewer {
    /// The viewer the flat `@user` value names. `Err` when it has no text `id`, a named field of the wrong kind, or a
    /// claim that is not JSON (a function, a non-finite number). A null claim is an absent one.
    pub fn from_user(user: &Value) -> Result<Viewer, String> {
        let Value::Object(map) = user else {
            return Err("viewer: @user is not a record".to_string());
        };
        let text = |key: &str| -> Result<Option<String>, String> {
            match map.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(text)) => Ok(Some(text.clone())),
                Some(_) => Err(format!("viewer: @user.{key} is not text")),
            }
        };
        let id = text("id")?
            .filter(|id| !id.is_empty())
            .ok_or("viewer: @user carries no id")?;
        let permissions = match map.get("permissions") {
            None | Some(Value::Null) => None,
            Some(Value::Array(items)) => Some(texts_of(items, "permissions")?),
            Some(_) => return Err("viewer: @user.permissions is not a list of text".to_string()),
        };
        let mut claims = Vec::new();
        for (name, value) in map.iter().filter(|(key, _)| !NAMED.contains(&key.as_str())) {
            if !matches!(value, Value::Null) {
                claims.push(Claim {
                    name: name.clone(),
                    value: json_of(value, name)?,
                });
            }
        }
        Ok(Viewer {
            id,
            email: text("email")?,
            name: text("name")?,
            role: text("role")?,
            permissions,
            claims,
        })
    }

    /// The flat `@user` value of this viewer. `Err` when a claim repeats a named field or another claim.
    pub fn to_user(&self) -> Result<Value, String> {
        let mut map = ValueMap::default();
        map.insert("id".to_string(), Value::String(self.id.clone()));
        for (key, field) in [
            ("email", &self.email),
            ("name", &self.name),
            ("role", &self.role),
        ] {
            if let Some(text) = field {
                map.insert(key.to_string(), Value::String(text.clone()));
            }
        }
        if let Some(permissions) = &self.permissions {
            map.insert(
                "permissions".to_string(),
                Value::Array(permissions.iter().cloned().map(Value::String).collect()),
            );
        }
        for claim in &self.claims {
            if map.contains_key(&claim.name) {
                return Err(format!("viewer claim {}: appears twice", claim.name));
            }
            let value: Value = serde_json::from_value(claim.value.clone())
                .map_err(|e| format!("viewer claim {}: {e}", claim.name))?;
            map.insert(claim.name.clone(), value);
        }
        Ok(Value::Object(map))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user(json: serde_json::Value) -> Value {
        serde_json::from_value(json).unwrap()
    }

    fn firebase_user() -> Value {
        user(json!({
            "id": "u1",
            "email": "a@b.co",
            "name": "Ada",
            "role": "admin",
            "permissions": ["read", "write"],
            "teamId": "t9",
            "approvalLimit": 5000,
            "email_verified": true,
            "amr": ["pwd"],
            "firebase": { "sign_in_provider": "password", "identities": { "email": ["a@b.co"] } },
            "tenant": null
        }))
    }

    #[test]
    fn claim_values_are_lossless() {
        let source = user(json!({
            "id": "u1",
            "org": { "team": { "unit": { "code": "x7", "size": 3, "tags": ["a", 1, true, null] } } },
            "scores": [1, 2],
            "grid": [[1, 2], [{ "k": "v" }]]
        }));
        let viewer = Viewer::from_user(&source).unwrap();
        assert_eq!(viewer.to_user().unwrap(), source);
        let wire = serde_json::to_value(&viewer).unwrap();
        assert!(wire["claims"].as_array().unwrap().contains(&json!({ "name": "scores", "value": [1, 2] })));
        let back: Viewer = serde_json::from_value(wire).unwrap();
        assert_eq!(back.to_user().unwrap(), source);
    }

    #[test]
    fn viewer_serde_round_trips_with_claims() {
        let viewer = Viewer::from_user(&firebase_user()).unwrap();
        let wire = serde_json::to_value(&viewer).unwrap();
        let claim = |name: &str| {
            wire["claims"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["name"] == name)
                .unwrap()["value"]
                .clone()
        };
        assert_eq!(claim("teamId"), json!("t9"));
        assert_eq!(claim("approvalLimit"), json!(5000));
        assert_eq!(claim("email_verified"), json!(true));
        assert_eq!(claim("amr"), json!(["pwd"]));
        assert_eq!(
            claim("firebase"),
            json!({ "sign_in_provider": "password", "identities": { "email": ["a@b.co"] } })
        );
        let back: Viewer = serde_json::from_value(wire).unwrap();
        assert_eq!(back, viewer);
    }

    #[test]
    fn viewer_wire_omits_absent_fields_and_defaults_claims() {
        let viewer: Viewer = serde_json::from_value(json!({ "id": "anonymous" })).unwrap();
        assert_eq!(viewer.claims, Vec::new());
        assert_eq!(viewer.role, None);
        assert_eq!(
            serde_json::to_value(&viewer).unwrap(),
            json!({ "id": "anonymous", "claims": [] })
        );
    }

    #[test]
    fn viewer_user_value_round_trips_and_drops_only_null_claims() {
        let viewer = Viewer::from_user(&firebase_user()).unwrap();
        assert_eq!(
            viewer.permissions,
            Some(vec!["read".to_string(), "write".to_string()])
        );
        assert!(
            viewer.claims.iter().all(|c| c.name != "tenant"),
            "a null claim is an absent one"
        );
        let mut expected = firebase_user();
        if let Value::Object(map) = &mut expected {
            map.shift_remove("tenant");
        }
        assert_eq!(viewer.to_user().unwrap(), expected);
    }

    #[test]
    fn viewer_refuses_what_no_claim_holds() {
        assert!(Viewer::from_user(&user(json!({ "role": "admin" })))
            .unwrap_err()
            .contains("no id"));
        assert!(Viewer::from_user(&user(json!({ "id": "" }))).is_err());
        assert!(Viewer::from_user(&user(json!({ "id": "u", "role": 3 })))
            .unwrap_err()
            .contains("@user.role"));
        assert!(Viewer::from_user(&user(json!({ "id": "u", "permissions": "all" }))).is_err());
        let mut nan = user(json!({ "id": "u" }));
        if let Value::Object(map) = &mut nan {
            map.insert("score".to_string(), Value::Array(vec![Value::Number(f64::NAN)]));
        }
        assert!(Viewer::from_user(&nan).unwrap_err().contains("score"));
        assert!(Viewer::from_user(&user(json!([]))).is_err());
    }

    #[test]
    fn viewer_to_user_refuses_a_claim_that_shadows_a_field() {
        let viewer = Viewer {
            id: "u".to_string(),
            email: None,
            name: None,
            role: Some("admin".to_string()),
            permissions: None,
            claims: vec![Claim {
                name: "role".to_string(),
                value: json!("owner"),
            }],
        };
        assert!(viewer.to_user().unwrap_err().contains("role"));
    }
}
