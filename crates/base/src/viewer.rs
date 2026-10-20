//! The viewer: who `@user` is, as one typed record. `@almadar/core`'s `Viewer` is generated from this type, and a
//! provider's claims are a typed list rather than open keys. The `@user` binding holds the same viewer flat (claims as
//! keys, a record claim as a nested object); [`Viewer::from_user`] and [`Viewer::to_user`] convert between the two.

use serde::{Deserialize, Serialize};

use crate::value::{Value, ValueMap};

/// One claim's value. A date travels as its ISO text.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ClaimValue {
    Text {
        text: String,
    },
    Number {
        number: f64,
    },
    Flag {
        flag: bool,
    },
    Texts {
        texts: Vec<String>,
    },
    /// A claim that is an object (Firebase's `firebase`, an OIDC `address`): its scalar leaves, each at its path.
    Record {
        entries: Vec<ClaimEntry>,
    },
}

/// A scalar leaf of a record claim.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ClaimLeaf {
    Text { text: String },
    Number { number: f64 },
    Flag { flag: bool },
    Texts { texts: Vec<String> },
}

/// One leaf of a record claim; `path` is the keys below the claim, outermost first.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimEntry {
    pub path: Vec<String>,
    pub value: ClaimLeaf,
}

/// One provider or identity claim beyond the named viewer fields.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub name: String,
    pub value: ClaimValue,
}

/// Who `@user` is.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Viewer {
    /// Stable subject identifier: the ownership key.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

fn leaf_of(value: &Value, at: &str) -> Result<Option<ClaimLeaf>, String> {
    Ok(Some(match value {
        Value::Null => return Ok(None),
        Value::String(text) => ClaimLeaf::Text { text: text.clone() },
        Value::Number(number) => ClaimLeaf::Number { number: *number },
        Value::Bool(flag) => ClaimLeaf::Flag { flag: *flag },
        Value::Array(items) => ClaimLeaf::Texts {
            texts: texts_of(items, at)?,
        },
        Value::Object(_) | Value::Lambda { .. } => {
            return Err(format!("viewer claim {at}: not a scalar"))
        }
    }))
}

fn entries_of(
    map: &ValueMap,
    path: &mut Vec<String>,
    out: &mut Vec<ClaimEntry>,
    at: &str,
) -> Result<(), String> {
    for (key, value) in map {
        path.push(key.clone());
        if let Value::Object(inner) = value {
            entries_of(inner, path, out, at)?;
        } else if let Some(leaf) = leaf_of(value, &format!("{at}.{}", path.join(".")))? {
            out.push(ClaimEntry {
                path: path.clone(),
                value: leaf,
            });
        }
        path.pop();
    }
    Ok(())
}

fn claim_of(name: &str, value: &Value) -> Result<Option<ClaimValue>, String> {
    if let Value::Object(map) = value {
        let mut entries = Vec::new();
        entries_of(map, &mut Vec::new(), &mut entries, name)?;
        return Ok(Some(ClaimValue::Record { entries }));
    }
    Ok(leaf_of(value, name)?.map(|leaf| match leaf {
        ClaimLeaf::Text { text } => ClaimValue::Text { text },
        ClaimLeaf::Number { number } => ClaimValue::Number { number },
        ClaimLeaf::Flag { flag } => ClaimValue::Flag { flag },
        ClaimLeaf::Texts { texts } => ClaimValue::Texts { texts },
    }))
}

fn leaf_value(leaf: &ClaimLeaf) -> Value {
    match leaf {
        ClaimLeaf::Text { text } => Value::String(text.clone()),
        ClaimLeaf::Number { number } => Value::Number(*number),
        ClaimLeaf::Flag { flag } => Value::Bool(*flag),
        ClaimLeaf::Texts { texts } => {
            Value::Array(texts.iter().cloned().map(Value::String).collect())
        }
    }
}

fn place(map: &mut ValueMap, path: &[String], leaf: Value, at: &str) -> Result<(), String> {
    let Some((last, outer)) = path.split_last() else {
        return Err(format!(
            "viewer claim {at}: a record entry has an empty path"
        ));
    };
    let mut here = map;
    for key in outer {
        let slot = here
            .entry(key.clone())
            .or_insert_with(|| Value::Object(ValueMap::new()));
        let Value::Object(inner) = slot else {
            return Err(format!(
                "viewer claim {at}: {key} is both a leaf and a record"
            ));
        };
        here = inner;
    }
    if here.insert(last.clone(), leaf).is_some() {
        return Err(format!(
            "viewer claim {at}: {} appears twice",
            path.join(".")
        ));
    }
    Ok(())
}

const NAMED: [&str; 5] = ["id", "email", "name", "role", "permissions"];

impl Viewer {
    /// The viewer the flat `@user` value names. `Err` when it has no text `id`, a named field of the wrong kind, or a
    /// claim no claim value holds (a list of non-text, a list or record inside a list). A null claim is an absent one.
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
            if let Some(value) = claim_of(name, value)? {
                claims.push(Claim {
                    name: name.clone(),
                    value,
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
        let mut map = ValueMap::new();
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
            let value = match &claim.value {
                ClaimValue::Text { text } => Value::String(text.clone()),
                ClaimValue::Number { number } => Value::Number(*number),
                ClaimValue::Flag { flag } => Value::Bool(*flag),
                ClaimValue::Texts { texts } => {
                    Value::Array(texts.iter().cloned().map(Value::String).collect())
                }
                ClaimValue::Record { entries } => {
                    let mut record = ValueMap::new();
                    for entry in entries {
                        place(
                            &mut record,
                            &entry.path,
                            leaf_value(&entry.value),
                            &claim.name,
                        )?;
                    }
                    Value::Object(record)
                }
            };
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
        assert_eq!(claim("teamId"), json!({ "kind": "text", "text": "t9" }));
        assert_eq!(
            claim("approvalLimit"),
            json!({ "kind": "number", "number": 5000.0 })
        );
        assert_eq!(
            claim("email_verified"),
            json!({ "kind": "flag", "flag": true })
        );
        assert_eq!(claim("amr"), json!({ "kind": "texts", "texts": ["pwd"] }));
        let firebase = claim("firebase");
        assert_eq!(firebase["kind"], "record");
        let entries = firebase["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.contains(&json!({ "path": ["sign_in_provider"], "value": { "kind": "text", "text": "password" } })));
        assert!(entries.contains(&json!({ "path": ["identities", "email"], "value": { "kind": "texts", "texts": ["a@b.co"] } })));
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
        assert!(
            Viewer::from_user(&user(json!({ "id": "u", "scores": [1, 2] })))
                .unwrap_err()
                .contains("scores")
        );
        assert!(Viewer::from_user(&user(json!({ "id": "u", "orgs": [{ "id": "o" }] }))).is_err());
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
                value: ClaimValue::Text {
                    text: "owner".to_string(),
                },
            }],
        };
        assert!(viewer.to_user().unwrap_err().contains("role"));
        let clash = Viewer {
            claims: vec![Claim {
                name: "x".to_string(),
                value: ClaimValue::Record {
                    entries: vec![
                        ClaimEntry {
                            path: vec!["a".to_string()],
                            value: ClaimLeaf::Flag { flag: true },
                        },
                        ClaimEntry {
                            path: vec!["a".to_string(), "b".to_string()],
                            value: ClaimLeaf::Flag { flag: true },
                        },
                    ],
                },
            }],
            role: None,
            ..viewer
        };
        assert!(clash
            .to_user()
            .unwrap_err()
            .contains("both a leaf and a record"));
    }
}
