//! A value that never prints: the type of every credential value the kernel and its hosts pass around.
//!
//! `Debug`, `Display` and `Serialize` show [`REDACTED`], so a log line, a trace, a journal dump or a derived `Debug` over a
//! struct holding one cannot leak it. Reading the value is the explicit [`Secret::expose`]; a wire type that must carry it
//! to a host says so with its own serializer (see `SecretText` in the embed wire).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// What a [`Secret`] shows in place of its value.
pub const REDACTED: &str = "<redacted>";

#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// The value. Every call site is a place the value leaves its wrapper on purpose.
    pub fn expose(&self) -> &T {
        &self.0
    }

    pub fn into_inner(self) -> T {
        self.0
    }
}

impl From<&str> for Secret<String> {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl<T> From<T> for Secret<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

impl<T> std::fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED)
    }
}

impl<T> std::fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED)
    }
}

impl<T> Serialize for Secret<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(REDACTED)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Secret<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Self)
    }
}

/// A [`Secret`] string on a wire: the one place its value is written out, as the plain JSON string a host reads.
/// `Debug` shows nothing, like the [`Secret`] it holds.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SecretText(Secret<String>);

impl SecretText {
    pub fn new(value: Secret<String>) -> Self {
        Self(value)
    }

    pub fn into_inner(self) -> Secret<String> {
        self.0
    }
}

impl Serialize for SecretText {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.expose())
    }
}

impl<'de> Deserialize<'de> for SecretText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|text| Self(Secret::new(text)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_prints_nowhere_and_exposes_only_on_request() {
        let secret = Secret::new("s3cret".to_string());
        assert_eq!(
            format!("{secret:?} {secret} {:?}", Some(&secret)),
            "<redacted> <redacted> Some(<redacted>)"
        );
        assert_eq!(serde_json::to_string(&secret).unwrap(), "\"<redacted>\"");
        assert_eq!(secret.expose(), "s3cret");
        assert_eq!(secret.clone().into_inner(), "s3cret");
        let bytes = Secret::new(vec![1u8, 2, 3]);
        assert_eq!(format!("{bytes:?}"), "<redacted>");
        assert_eq!(serde_json::to_string(&bytes).unwrap(), "\"<redacted>\"");
    }

    #[test]
    fn a_wire_secret_writes_its_value_and_still_prints_nothing() {
        let wire = SecretText::new(Secret::new("s3cret".to_string()));
        assert_eq!(serde_json::to_string(&wire).unwrap(), "\"s3cret\"");
        assert_eq!(format!("{wire:?}"), "SecretText(<redacted>)");
        let read: SecretText = serde_json::from_str("\"s3cret\"").unwrap();
        assert_eq!(read.into_inner().expose(), "s3cret");
    }

    #[test]
    fn a_secret_reads_its_value_and_a_struct_holding_one_never_leaks_it() {
        #[derive(Debug, Serialize, Deserialize)]
        struct Holder {
            name: String,
            value: Secret<String>,
        }
        let held: Holder = serde_json::from_str(r#"{"name":"n","value":"s3cret"}"#).unwrap();
        assert_eq!(held.value.expose(), "s3cret");
        assert!(!format!("{held:?}").contains("s3cret"));
        assert!(!serde_json::to_string(&held).unwrap().contains("s3cret"));
        assert!(
            format!("{held:?}").contains("\"n\""),
            "control: the rest of the struct shows"
        );
        assert_eq!(Secret::new(1) == Secret::new(1), true);
        assert!(
            Secret::new(1) != Secret::new(2),
            "control: equality compares the values"
        );
    }
}
