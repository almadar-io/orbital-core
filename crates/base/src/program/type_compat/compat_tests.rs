use super::{
    compat_kind_oir, oir_type_accepts_kind, oir_type_satisfies, oir_type_satisfies_field,
    set_value_type_compatible, Coercion, SemanticStringKind, WidenKind,
};
use crate::ir::types::OirType;
use crate::ir::types::OirType::*;

type C = Coercion;

// The legacy string-level acceptance set — pinned as booleans so the
// wrapper provably preserves the predicate every call site relied on.
#[test]
fn equal_and_required_marker() {
    assert!(set_value_type_compatible("string", "string"));
    assert!(set_value_type_compatible("string!", "string"));
    assert!(set_value_type_compatible("number", "number!"));
}

#[test]
fn any_is_not_a_type() {
    assert!(!set_value_type_compatible("any", "object"));
    assert!(!set_value_type_compatible("any", "number"));
    assert!(!set_value_type_compatible("string", "any"));
    assert!(!set_value_type_compatible("json", "object"));
    assert!(!set_value_type_compatible("object", "json"));
    assert!(
        set_value_type_compatible("string", "string"),
        "control: a declared type still accepts itself"
    );
}

#[test]
fn date_family_accepts_string_and_no_number() {
    assert!(set_value_type_compatible("date", "string"));
    assert!(set_value_type_compatible("timestamp", "string"));
    assert!(!set_value_type_compatible("timestamp", "number"));
    assert!(!set_value_type_compatible("date", "number"));
    assert!(!set_value_type_compatible("date", "boolean"));
    // `timestamp` does not cross-accept `date`/`datetime` names.
    assert!(!set_value_type_compatible("date", "timestamp"));
    assert!(!set_value_type_compatible("datetime", "timestamp"));
}

#[test]
fn entity_and_named_accept_object_or_string() {
    assert!(set_value_type_compatible("entity", "object"));
    assert!(set_value_type_compatible("relation", "string"));
    assert!(set_value_type_compatible("Article", "object"));
    assert!(set_value_type_compatible("Article", "string"));
    assert!(!set_value_type_compatible("Article", "number"));
    assert!(!set_value_type_compatible("Article", "json"));
    // Distinct named types do not inter-assign.
    assert!(!set_value_type_compatible("Article", "Comment"));
    // Reverse directions are rejected.
    assert!(!set_value_type_compatible("object", "Article"));
    assert!(!set_value_type_compatible("string", "Article"));
}

#[test]
fn array_kinds_compatible_across_elements() {
    assert!(set_value_type_compatible("array", "[CacheEntry]"));
    assert!(set_value_type_compatible("[Foo]", "array"));
    assert!(set_value_type_compatible("[Foo]", "[Bar]"));
    assert!(!set_value_type_compatible("string", "[Foo]"));
    assert!(!set_value_type_compatible("object", "[Foo]"));
    assert!(!set_value_type_compatible("[Foo]", "object"));
    // A node accepts an array of pattern trees.
    assert!(set_value_type_compatible("node", "[Foo]"));
}

#[test]
fn oir_type_satisfies_field_keeps_inferred_element_type() {
    let string_array = Array {
        inner: Box::new(String),
    };
    let number_array = Array {
        inner: Box::new(Number),
    };
    let unknown_array = Array {
        inner: Box::new(OirType::erased()),
    };
    // A concrete inferred element type compares structurally, not by the
    // erased "array" kind — this is the 4.2-follow-up fix: the inferred
    // side is no longer round-tripped through `oir_type_kind`.
    assert!(oir_type_satisfies_field(&string_array, "[string]"));
    assert!(!oir_type_satisfies_field(&number_array, "[string]"));
    // Unknown elements retain a known array kind and fit typed array slots.
    assert!(oir_type_satisfies_field(&unknown_array, "[string]"));
    // The nullable outer container needs refinement.
    let opt_string_array = Optional {
        inner: Box::new(string_array.clone()),
    };
    assert!(!oir_type_satisfies_field(&opt_string_array, "[string]"));
}

// `oir_type_satisfies` / `oir_type_accepts_kind` are the structured
// siblings of `oir_type_satisfies_field` / `set_value_type_compatible` —
// same acceptance set, declared side passed as a real `OirType` instead
// of a kind-string (the `field_shape` callers' shape).
#[test]
fn oir_type_satisfies_mirrors_the_str_wrapper() {
    let string_array = Array {
        inner: Box::new(String),
    };
    let number_array = Array {
        inner: Box::new(Number),
    };
    assert!(oir_type_satisfies(
        &string_array,
        &Array {
            inner: Box::new(String)
        }
    ));
    assert!(!oir_type_satisfies(
        &number_array,
        &Array {
            inner: Box::new(String)
        }
    ));
    // Unknowable inferred types never reject, declared side real or not.
    assert!(oir_type_satisfies(
        &OirType::erased(),
        &Array {
            inner: Box::new(String)
        }
    ));
    assert!(oir_type_satisfies(&SExpr, &Number));
}

#[test]
fn oir_type_accepts_kind_mirrors_the_str_wrapper() {
    assert!(oir_type_accepts_kind(
        &Array {
            inner: Box::new(String)
        },
        "[string]"
    ));
    assert!(!oir_type_accepts_kind(
        &Array {
            inner: Box::new(String)
        },
        "[number]"
    ));
    // A real `Reference` declared side still takes a row or an id string.
    let article = Reference {
        entity: "Article".into(),
    };
    assert!(oir_type_accepts_kind(&article, "string"));
    assert!(oir_type_accepts_kind(&article, "object"));
    assert!(!oir_type_accepts_kind(&article, "number"));
    assert!(oir_type_accepts_kind(&Money, "money"));
    assert!(oir_type_accepts_kind(&Money, "number"));
    assert!(oir_type_accepts_kind(&File, "file"));
    assert!(oir_type_accepts_kind(&File, "object"));
    // An erased-element actual never rejects.
    assert!(oir_type_accepts_kind(
        &Array {
            inner: Box::new(OirType::erased())
        },
        "[opaque]"
    ));
    assert!(oir_type_accepts_kind(
        &Array {
            inner: Box::new(Array {
                inner: Box::new(Number)
            })
        },
        "[[float]]"
    ));
}

#[test]
fn compat_kind_oir_round_trips_the_legacy_vocabulary() {
    assert_eq!(compat_kind_oir("string"), String);
    assert_eq!(compat_kind_oir("number!"), Number);
    assert_eq!(
        compat_kind_oir("[string]"),
        Array {
            inner: Box::new(String)
        }
    );
    // Markers with no real `OirType` fall back to their witness type.
    assert_eq!(compat_kind_oir("money"), Money);
    assert_eq!(compat_kind_oir("any"), OirType::erased(), "a spelling nothing declares is unknown");
}

#[test]
fn object_json_map_family() {
    assert!(!set_value_type_compatible("object", "json"), "json is not a type");
    assert!(!set_value_type_compatible("json", "object"), "json is not a type");
    assert!(set_value_type_compatible("Map<string,string>", "object"));
    assert!(set_value_type_compatible("object", "Map<string,string>"));
    assert!(set_value_type_compatible("Map<a,b>", "Map<c,d>"));
    // Raw `Map<...>` tags also accept a string actual (the legacy
    // predicate's PascalCase arm) — a quirk the relation preserves.
    assert!(set_value_type_compatible("Map<string,string>", "string"));
    assert!(!set_value_type_compatible("Map<string,string>", "number"));
}

#[test]
fn a_tagged_union_declaration_takes_an_object_and_rejects_a_primitive() {
    assert!(set_value_type_compatible("union", "object"));
    assert!(!set_value_type_compatible("union", "string"));
    assert!(!set_value_type_compatible("union", "number"));
    assert!(!set_value_type_compatible("union", "array"));
}

#[test]
fn scalar_is_not_a_type() {
    // `scalar` has no meaning in the kind vocabulary: identity only, it
    // admits no member kind and fits no declared kind.
    assert!(set_value_type_compatible("scalar", "scalar"));
    assert!(!set_value_type_compatible("scalar", "string"));
    assert!(!set_value_type_compatible("scalar", "number"));
    assert!(!set_value_type_compatible("scalar", "boolean"));
    assert!(!set_value_type_compatible("scalar", "null"));
    assert!(!set_value_type_compatible("object", "scalar"));
    assert!(!set_value_type_compatible("json", "scalar"));
    assert!(!set_value_type_compatible("string", "scalar"));
    // Raw int/float are identity-only declared kinds.
    assert!(set_value_type_compatible("int", "int"));
    assert!(!set_value_type_compatible("number", "int"));
    assert!(!set_value_type_compatible("int", "number"));
}

#[test]
fn bytes_is_not_implicitly_a_string() {
    assert!(set_value_type_compatible("bytes", "bytes"));
    assert!(!set_value_type_compatible("bytes", "string"));
    assert!(!set_value_type_compatible("string", "bytes"));
    assert!(!set_value_type_compatible("number", "bytes"));
    assert!(!set_value_type_compatible("any", "bytes"), "any is not a type");
    assert!(super::sub(&Bytes, &Bytes).is_some());
    assert_eq!(super::sub(&String, &Bytes), None);
    assert_eq!(super::sub(&Bytes, &String), None);
    assert_eq!(super::oir_type_kind(&Bytes), "bytes");
}

#[test]
fn semantic_refinements_and_money_file() {
    assert!(set_value_type_compatible("email", "string"));
    assert!(set_value_type_compatible("url", "string"));
    assert!(set_value_type_compatible("phone", "string"));
    assert!(set_value_type_compatible("uuid", "string"));
    assert!(set_value_type_compatible("image", "string"));
    assert!(!set_value_type_compatible("email", "number"));
    // Semantic domains do not inter-assign; a semantic value widens to string (owner ruling, P-009).
    assert!(!set_value_type_compatible("email", "url"));
    assert!(set_value_type_compatible("string", "email"));
    assert!(set_value_type_compatible("money", "number"));
    assert!(!set_value_type_compatible("number", "money"));
    assert!(set_value_type_compatible("file", "object"));
    assert!(!set_value_type_compatible("file", "string"));
}

#[test]
fn node_accepts_scalars() {
    // The arm moved in from config_type.rs: every scalar renders.
    assert!(set_value_type_compatible("node", "string"));
    assert!(set_value_type_compatible("node", "number"));
    assert!(set_value_type_compatible("node", "boolean"));
    assert!(set_value_type_compatible("node", "object"));
    assert!(!set_value_type_compatible("node", "json"));
    assert!(set_value_type_compatible("node", "number!"));
    assert!(!set_value_type_compatible("string", "node"));
}

#[test]
fn clear_contradictions_rejected() {
    assert!(!set_value_type_compatible("string", "number"));
    assert!(!set_value_type_compatible("boolean", "string"));
    assert!(!set_value_type_compatible("json", "string"));
    assert!(!set_value_type_compatible("json", "array"));
    assert!(!set_value_type_compatible("entity", "json"));
    assert!(!set_value_type_compatible("date", "email"));
}

// The relation itself: each acceptance above pinned to its exact witness,
// so the remaining leniency is enumerated here, not implicit.
#[test]
fn sub_identity_and_numeric_widening() {
    use super::sub;
    assert_eq!(sub(&String, &String), Some(C::Identity));
    // Integer -> Number widens; Number -> Integer does not narrow (4.4).
    assert_eq!(
        sub(&Integer, &Number),
        Some(C::Widen(WidenKind::IntegerNumber))
    );
    assert_eq!(sub(&Number, &Integer), None);
    assert_eq!(sub(&Money, &Integer), None);
    // Money <-> Number stay mutually compatible; Integer -> Money widens.
    assert_eq!(sub(&Money, &Number), Some(C::Widen(WidenKind::MoneyNumber)));
    assert_eq!(sub(&Number, &Money), Some(C::Widen(WidenKind::MoneyNumber)));
    assert_eq!(
        sub(&Integer, &Money),
        Some(C::Widen(WidenKind::MoneyNumber))
    );
    assert_eq!(sub(&Boolean, &Boolean), Some(C::Identity));
    assert_eq!(sub(&Boolean, &Number), None);
    assert_eq!(sub(&Number, &String), None);
    assert_eq!(sub(&String, &Number), None);
}

#[test]
fn sub_optional_wrap_unwrap_witnesses() {
    use super::sub;
    assert_eq!(
        sub(
            &String,
            &Optional {
                inner: Box::new(String)
            }
        ),
        Some(C::Wrap(Box::new(C::Identity)))
    );
    assert_eq!(
        sub(
            &Optional {
                inner: Box::new(String)
            },
            &String
        ),
        Some(C::Unwrap {
            checked: false,
            inner: Box::new(C::Identity)
        })
    );
    assert_eq!(
        sub(
            &Optional {
                inner: Box::new(Integer)
            },
            &Number
        ),
        Some(C::Unwrap {
            checked: false,
            inner: Box::new(C::Widen(WidenKind::IntegerNumber))
        })
    );
    assert_eq!(
        sub(
            &Optional {
                inner: Box::new(String)
            },
            &Optional {
                inner: Box::new(String)
            }
        ),
        Some(C::Wrap(Box::new(C::Identity)))
    );
    assert_eq!(
        sub(
            &Optional {
                inner: Box::new(Number)
            },
            &String
        ),
        None
    );
}

#[test]
fn sub_string_kind_family() {
    use super::sub;
    let status = Enum {
        name: "Status".into(),
    };
    // Enum/Trait erase to "string": kind-level equality with String.
    assert_eq!(sub(&String, &status), Some(C::Identity));
    assert_eq!(sub(&status, &String), Some(C::Identity));
    assert_eq!(sub(&Trait, &status), Some(C::Identity));
    // Semantic refinements accept any string-kind source.
    assert_eq!(
        sub(&String, &Email),
        Some(C::Refine(SemanticStringKind::Email))
    );
    assert_eq!(sub(&status, &Url), Some(C::Refine(SemanticStringKind::Url)));
    // …a semantic domain is a string, but domains do not inter-assign.
    assert_eq!(sub(&Email, &String), Some(C::Widen(WidenKind::SemanticToString)));
    assert_eq!(sub(&Email, &Url), None);
    assert_eq!(sub(&Email, &Email), Some(C::Identity));
}

#[test]
fn call_key_is_nominal_a_bare_string_never_assigns_but_a_call_key_widens() {
    use super::{assignable, castable, sub};
    assert_eq!(sub(&String, &CallKey), Some(C::Explicit));
    assert_eq!(sub(&CallKey, &String), Some(C::Identity));
    assert_eq!(sub(&CallKey, &CallKey), Some(C::Identity));
    assert_eq!(sub(&Number, &CallKey), None);
    assert!(
        !assignable(&String, &CallKey),
        "a bare string is not assignable"
    );
    assert!(assignable(&CallKey, &String));
    assert!(assignable(&CallKey, &CallKey));
    assert!(!oir_type_satisfies(&String, &CallKey));
    assert!(oir_type_satisfies(&CallKey, &String));
    assert!(!set_value_type_compatible("CallKey", "string"));
    assert!(!set_value_type_compatible("callkey", "string"));
    assert!(set_value_type_compatible("string", "CallKey"));
    assert!(set_value_type_compatible("callkey", "callkey"));
    assert!(
        !set_value_type_compatible("bytes", "string"),
        "control: bytes unchanged"
    );
    assert!(
        set_value_type_compatible("email", "string"),
        "control: semantic domains still refine"
    );
    assert!(castable(&String, &CallKey));
    assert!(castable(&CallKey, &String));
    assert!(!castable(&Number, &CallKey));
}

#[test]
fn an_optional_or_listed_call_key_slot_still_needs_the_cast() {
    use super::assignable;
    use crate::ir::types::OirType as T;
    let optional = Optional {
        inner: Box::new(CallKey),
    };
    assert!(!assignable(&String, &optional));
    assert!(assignable(&CallKey, &optional));
    let keys = T::array(CallKey);
    assert!(!assignable(&T::array(String), &keys));
    assert!(assignable(&T::array(CallKey), &keys));
}

#[test]
fn sub_date_family_and_scalars() {
    use super::sub;
    assert_eq!(sub(&String, &Date), Some(C::Widen(WidenKind::StringToDate)));
    assert_eq!(sub(&Integer, &DateTime), None);
    assert_eq!(sub(&Number, &DateTime), None);
    assert_eq!(sub(&Money, &Date), None);
    assert_eq!(sub(&Number, &Date), None);
    assert_eq!(sub(&Instant, &Number), None);
    assert_eq!(sub(&Integer, &Instant), None);
    assert_eq!(sub(&String, &DateTime), Some(C::Widen(WidenKind::StringToDateTime)));
    assert_eq!(sub(&Date, &DateTime), None);
    assert_eq!(sub(&DateTime, &Date), None);
    assert_eq!(sub(&Boolean, &Date), None);
}

#[test]
fn a_primitive_union_takes_its_member_kinds_and_nothing_else() {
    use super::sub;
    let union = Union {
        variants: vec![
            crate::ir::types::OirField::new("string".to_string(), String),
            crate::ir::types::OirField::new("number".to_string(), Number),
        ].into(),
    };
    assert_eq!(
        sub(&String, &union),
        Some(C::Widen(WidenKind::ToUnionMember))
    );
    assert_eq!(
        sub(&Number, &union),
        Some(C::Widen(WidenKind::ToUnionMember))
    );
    assert_eq!(
        sub(&Integer, &union),
        Some(C::Widen(WidenKind::ToUnionMember))
    );
    assert_eq!(sub(&Boolean, &union), None, "boolean is not a member");
    // A tagged union of structs takes no primitive.
    let tagged = Union {
        variants: vec![crate::ir::types::OirField::new(
            "a".to_string(),
            Struct { fields: Default::default(), additional: None },
        )].into(),
    };
    assert_eq!(sub(&String, &tagged), None);
}

#[test]
fn sub_object_json_family_mutually_compatible() {
    use super::sub;
    let row = Struct { fields: Default::default(), additional: None };
    let bag = Map {
        key: Box::new(String),
        value: Box::new(OirType::erased()),
    };
    assert_eq!(sub(&row, &bag), Some(C::Erase { from: row.clone() }));
    assert_eq!(sub(&bag, &row), Some(C::Erase { from: bag.clone() }));
    assert_eq!(sub(&SExpr, &row), Some(C::Erase { from: SExpr }));
    assert_eq!(sub(&row, &SExpr), Some(C::Erase { from: row.clone() }));
    assert_eq!(sub(&File, &row), Some(C::Erase { from: File }));
    // an expression does not leak into string/array-declared slots.
    assert_eq!(sub(&SExpr, &String), None);
    assert_eq!(
        sub(
            &SExpr,
            &Array {
                inner: Box::new(SExpr)
            }
        ),
        None
    );
}

#[test]
fn an_atom_and_a_list_of_program_data_are_an_sexpr() {
    use super::sub;
    let list = |inner: crate::ir::types::OirType| Array {
        inner: Box::new(inner),
    };
    for atom in [String, Number, Boolean] {
        assert!(sub(&atom, &SExpr).is_some(), "{atom:?} is an atom");
    }
    assert!(
        sub(&list(SExpr), &SExpr).is_some(),
        "a list of expressions is an expression"
    );
    assert!(sub(&list(String), &SExpr).is_some());
    assert!(sub(&list(list(Number)), &SExpr).is_some());
    assert!(sub(
        &Tuple {
            items: vec![String, Number, list(SExpr)]
        },
        &SExpr
    )
    .is_some());
    assert!(
        sub(&Tuple { items: vec![] }, &SExpr).is_some(),
        "the empty list"
    );
    assert!(
        sub(&Struct { fields: Default::default(), additional: None }, &SExpr).is_some(),
        "an options object"
    );
    assert!(sub(
        &Optional {
            inner: Box::new(String)
        },
        &SExpr
    )
    .is_some());
    assert_eq!(
        sub(&SExpr, &String),
        None,
        "control: an expression is not a string"
    );
    assert_eq!(
        sub(&SExpr, &list(SExpr)),
        None,
        "control: nor a list, until it is read as one"
    );
    assert_eq!(
        sub(&list(Date), &SExpr),
        None,
        "control: a list holding a date is not program data"
    );
    assert_eq!(
        sub(&String, &OirType::erased()),
        None,
        "control: only the expression type takes atoms"
    );
}

#[test]
fn sub_arrays_structural() {
    use super::sub;
    let a = Array {
        inner: Box::new(String),
    };
    // Identical array: the top-level Identity shortcut answers before
    // the structural Array arm is consulted.
    assert_eq!(sub(&a, &a.clone()), Some(C::Identity));
    // Coercible element: the element witness is lifted (string-kind →
    // enum is the kind-level identity the Enum arm admits).
    let b = Array {
        inner: Box::new(Enum { name: "E".into() }),
    };
    assert_eq!(sub(&a, &b), Some(C::Element(Box::new(C::Identity))));
    // Element-incompatible arrays reject outright (Phase 4.2).
    let c = Array {
        inner: Box::new(Number),
    };
    assert_eq!(sub(&a, &c), None);
    assert_eq!(sub(&a, &String), None);
    assert_eq!(sub(&SExpr, &a), None);
}

#[test]
fn sub_reference_row_or_id() {
    use super::sub;
    let article = Reference {
        entity: "Article".into(),
    };
    let row = Struct { fields: Default::default(), additional: None };
    assert_eq!(sub(&row, &article), Some(C::Row { from_id: false }));
    assert_eq!(sub(&String, &article), Some(C::Row { from_id: true }));
    assert_eq!(
        sub(&Enum { name: "S".into() }, &article),
        Some(C::Row { from_id: true })
    );
    // Distinct references do not inter-assign; entities do not escape.
    assert_eq!(
        sub(
            &Reference {
                entity: "Comment".into()
            },
            &article
        ),
        None
    );
    // Owner ruling 2026-10-25: a relation read is the related row's id, so it fits a string, and is not a row.
    assert_eq!(sub(&article, &String), Some(C::Erase { from: article.clone() }));
    assert_eq!(sub(&article, &row), None);
    assert_eq!(sub(&SExpr, &article), None);
}

#[test]
fn sub_row_is_object_family() {
    use super::sub;
    let row = Row {
        entity: "Revision".into(),
    };
    let revision_ref = Reference {
        entity: "Revision".into(),
    };
    let object = Struct { fields: Default::default(), additional: None };
    // A row takes an object-family value and erases into shapeless destinations.
    assert_eq!(
        sub(&object, &row),
        Some(C::Erase {
            from: object.clone()
        })
    );
    assert_eq!(sub(&SExpr, &row), Some(C::Erase { from: SExpr }));
    assert_eq!(sub(&row, &object), Some(C::Erase { from: row.clone() }));
    assert_eq!(sub(&row, &row), Some(C::Identity));
    // A reference takes a loaded row.
    assert_eq!(sub(&row, &revision_ref), Some(C::Row { from_id: false }));
    // Controls: a Row is not an id, and an id is not a Row.
    assert_eq!(sub(&String, &row), None);
    assert_eq!(sub(&row, &String), None);
    assert_eq!(sub(&revision_ref, &row), None);
    assert_eq!(crate::type_compat::oir_type_kind(&row), "object");
}

#[test]
fn a_semantic_string_domain_widens_to_string_and_never_back_unchecked() {
    use super::sub;
    for domain in [Email, Url, Phone, Uuid, Image] {
        assert_eq!(
            sub(&domain, &String),
            Some(C::Widen(WidenKind::SemanticToString)),
            "{domain:?}"
        );
        assert!(sub(&String, &domain).is_some_and(|c| matches!(c, C::Refine(_))));
        assert_eq!(sub(&domain, &Number), None, "control: a domain is not a number");
    }
    assert_eq!(sub(&Email, &Url), None, "control: domains are not interchangeable");
    assert_eq!(sub(&Email, &Email), Some(C::Identity));
}

#[test]
fn sub_node_renders_any_scalar() {
    use super::sub;
    assert_eq!(sub(&String, &Node), Some(C::Widen(WidenKind::ScalarToNode)));
    assert_eq!(sub(&Number, &Node), Some(C::Widen(WidenKind::ScalarToNode)));
    assert_eq!(
        sub(&Boolean, &Node),
        Some(C::Widen(WidenKind::ScalarToNode))
    );
    assert_eq!(sub(&Money, &Node), Some(C::Widen(WidenKind::ScalarToNode)));
    let row = Struct { fields: Default::default(), additional: None };
    assert_eq!(sub(&row, &Node), Some(C::Erase { from: row }));
    let arr = Array {
        inner: Box::new(String),
    };
    assert_eq!(sub(&arr, &Node), Some(C::Erase { from: arr }));
    // an expression is not renderable content.
    assert_eq!(sub(&SExpr, &Node), None);
    assert_eq!(sub(&Node, &String), None);
}
