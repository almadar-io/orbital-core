//! Sealed types: records only a kernel operator can make (EXEC-IDENTITY-2). A sealed type is nominal: the checker
//! accepts a value of it only from an expression of the same type, so a literal, a parse or a cast never fits, and the
//! runtime carries it as `Value::Sealed`, which no wire form deserializes to.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::ir::types::{OirField, OirType};

/// A credential `jws/verify` checked: who it names, who issued it, for whom, every claim, and when it stops holding.
pub const VERIFIED: &str = "Verified";

/// Every sealed type name.
pub const SEALED_TYPES: [&str; 1] = [VERIFIED];

pub fn is_sealed_type(name: &str) -> bool {
    SEALED_TYPES.contains(&name)
}

/// The type a sealed record is declared as.
pub fn sealed_type_ref(name: &str) -> OirType {
    OirType::TypeRef { name: name.to_string() }
}

/// The name of the sealed type `ty` is, when it is one.
pub fn sealed_name(ty: &OirType) -> Option<&str> {
    match ty {
        OirType::TypeRef { name } if is_sealed_type(name) => Some(name),
        _ => None,
    }
}

/// The fields a read through the sealed type `name` finds (`@verified.subject`).
pub fn sealed_record(name: &str) -> Option<OirType> {
    match name {
        VERIFIED => Some(OirType::Struct {
            fields: vec![
                OirField::new("subject", OirType::String),
                OirField::new("issuer", OirType::String),
                OirField::new("audience", OirType::String),
                OirField::new(
                    "claims",
                    OirType::array(OirType::Struct {
                        fields: vec![
                            OirField::new("name", OirType::String),
                            OirField::new("value", OirType::TypeRef { name: "JsonValue".to_string() }),
                        ]
                        .into(),
                        additional: None,
                    }),
                ),
                OirField::new("expiresAt", OirType::Instant),
            ]
            .into(),
            additional: None,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_verified_is_sealed_and_it_has_a_record() {
        assert!(is_sealed_type("Verified"));
        assert!(!is_sealed_type("Viewer"));
        assert!(sealed_record("Verified").is_some());
        assert!(sealed_record("Viewer").is_none());
        assert_eq!(sealed_name(&sealed_type_ref("Verified")), Some("Verified"));
        assert_eq!(sealed_name(&OirType::String), None);
    }
}
