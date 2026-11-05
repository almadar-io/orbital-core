//! Sealed types: records only a kernel operator can make (EXEC-IDENTITY-2). A sealed type is nominal: the checker
//! accepts a value of it only from an expression of the same type, so a literal, a parse or a cast never fits, and the
//! runtime carries it as `Value::Sealed`, which no wire form deserializes to.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::ir::types::{OirField, OirType};

/// A credential `jws/verify` checked: who it names, who issued it, for whom, every claim, and when it stops holding.
pub const VERIFIED: &str = "Verified";

/// A region of kernel memory `mem/arena-new` made: its id, strategy (`bump`, `stack` or `shared`) and size in bytes; a
/// shared arena a guest adopted also lists the owner's committed blocks, in the order the owner made them.
pub const ARENA: &str = "Arena";

/// A span `mem/alloc` took from an arena: its id, the arena's id and its length in bytes.
pub const BLOCK: &str = "Block";

/// A live hold `mem/borrow` took on a block: its id, the block's id and the mode (`shared` or `exclusive`).
pub const BORROW: &str = "Borrow";

/// Every sealed type name.
pub const SEALED_TYPES: [&str; 4] = [VERIFIED, ARENA, BLOCK, BORROW];

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
                            OirField::new("value", claim_value_type()),
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
        ARENA => Some(sealed_struct(vec![
            OirField::new("id", OirType::Integer),
            OirField::new("kind", OirType::String),
            OirField::new("capacity", OirType::Integer),
            OirField::optional("blocks", OirType::array(sealed_type_ref(BLOCK))),
        ])),
        BLOCK => Some(sealed_struct(vec![
            OirField::new("id", OirType::Integer),
            OirField::new("arena", OirType::Integer),
            OirField::new("length", OirType::Integer),
        ])),
        BORROW => Some(sealed_struct(vec![
            OirField::new("id", OirType::Integer),
            OirField::new("block", OirType::Integer),
            OirField::new("mode", OirType::String),
        ])),
        _ => None,
    }
}

/// The closed type of a `Verified` claim's value: `string | number | boolean | [string]`.
pub fn claim_value_type() -> OirType {
    OirType::Union {
        variants: vec![
            OirField::new("string", OirType::String),
            OirField::new("number", OirType::Number),
            OirField::new("boolean", OirType::Boolean),
            OirField::new("array", OirType::array(OirType::String)),
        ]
        .into(),
    }
}

fn sealed_struct(fields: Vec<OirField>) -> OirType {
    OirType::Struct { fields: fields.into(), additional: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mem_handles_are_sealed_with_their_records() {
        for (name, fields) in [
            ("Arena", &["id", "kind", "capacity", "blocks"][..]),
            ("Block", &["id", "arena", "length"][..]),
            ("Borrow", &["id", "block", "mode"][..]),
        ] {
            assert!(is_sealed_type(name), "{name}");
            assert_eq!(sealed_name(&sealed_type_ref(name)), Some(name));
            let Some(OirType::Struct { fields: found, .. }) = sealed_record(name) else { panic!("{name} has a record") };
            let names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
            assert_eq!(names, fields, "{name}");
        }
        assert!(!is_sealed_type("Arenas"), "control: a near name is not sealed");
        assert!(sealed_record("Arenas").is_none());
    }

    #[test]
    fn only_verified_is_sealed_and_it_has_a_record() {
        assert!(is_sealed_type("Verified"));
        assert!(!is_sealed_type("Viewer"));
        assert!(sealed_record("Verified").is_some());
        assert!(sealed_record("Viewer").is_none());
        assert_eq!(sealed_name(&sealed_type_ref("Verified")), Some("Verified"));
        assert_eq!(sealed_name(&OirType::String), None);
    }

    #[test]
    fn a_verified_claim_value_is_the_closed_claim_union() {
        let Some(OirType::Struct { fields, .. }) = sealed_record("Verified") else { panic!("Verified has a record") };
        let claims = fields.iter().find(|f| f.name == "claims").expect("claims");
        let OirType::Array { inner: items } = &claims.field_type else { panic!("claims is a list: {:?}", claims.field_type) };
        let OirType::Struct { fields: claim, .. } = items.as_ref() else { panic!("a claim is a record") };
        let value = claim.iter().find(|f| f.name == "value").expect("value");
        let OirType::Union { variants } = &value.field_type else { panic!("value is a union: {:?}", value.field_type) };
        let arms: Vec<(&str, &OirType)> = variants.iter().map(|v| (v.name.as_str(), &v.field_type)).collect();
        assert_eq!(
            arms,
            vec![
                ("string", &OirType::String),
                ("number", &OirType::Number),
                ("boolean", &OirType::Boolean),
                ("array", &OirType::array(OirType::String)),
            ]
        );
        assert!(!format!("{:?}", sealed_record("Verified")).contains("TypeRef"), "control: no undeclared type is named");
    }
}
