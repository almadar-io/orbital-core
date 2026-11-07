//! Cross-backend `OirValue` default → target-literal rendering.
//!
//! Mirrors `ir/render.rs`'s split of "walk the tree once, in orbital-core"
//! from "each backend supplies only its leaves" — but for a declared field's
//! `default: Option<OirValue>` against its `OirType`, instead of the type
//! itself. [`render_default`] owns the dispatch; a backend supplies only
//! [`TargetLiteralRenderer`]'s leaf renderers.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::ir::types::{OirType, OirValue};

/// Renders one target-language literal per `OirValue` kind. `Out` mirrors
/// [`super::TargetTypeRenderer::Out`] — `String` for TS/Python/Kotlin/Swift,
/// a token stream for Rust.
pub trait TargetLiteralRenderer {
    type Out;

    fn null(&self) -> Self::Out;
    fn bool(&self, b: bool) -> Self::Out;
    fn integer(&self, i: i64) -> Self::Out;
    fn number(&self, n: f64) -> Self::Out;
    fn string(&self, s: &str) -> Self::Out;
    fn enum_member(&self, enum_name: &str, member: &str) -> Self::Out;
    fn array(&self, items: Vec<Self::Out>) -> Self::Out;
    /// Struct/map/json defaults: a shell may decline (`None` = emit no default).
    fn object(&self) -> Option<Self::Out> {
        None
    }
    /// A `Map K V` default: its entries in written order, each value rendered at `V`, or `None` when the shell cannot write one.
    fn map(&self, _entries: Vec<(String, Self::Out)>) -> Option<Self::Out> {
        None
    }
    /// A `node` default that is text (a node can be text): the target's node value for it, or `None` when the shell cannot write one (the default is then refused).
    fn node_text(&self, _s: &str) -> Option<Self::Out> {
        None
    }
    /// A union default: `inner` is the value rendered at the union arm `arm` it fits. Untagged targets keep `inner`.
    fn union_arm(&self, _arm: &str, inner: Self::Out) -> Self::Out {
        inner
    }
}

/// A declared default's `OirValue` doesn't fit the field's `OirType` — e.g. a
/// `Boolean` field defaulting to a `String` value. The validator's job to
/// reject upstream; this is the fallback so a renderer never silently
/// fabricates a leaf for a shape it wasn't given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiteralMismatch {
    pub expected: String,
    pub found: &'static str,
}

/// Greppable text a shell embeds where a default could not be rendered, so the
/// target compiler fails loudly; `orbital-compiler` refuses to write files carrying it.
pub const LITERAL_TYPE_MISMATCH_MARKER: &str = "LITERAL_TYPE_MISMATCH:";

impl LiteralMismatch {
    pub fn marker(&self) -> String {
        format!("{LITERAL_TYPE_MISMATCH_MARKER} expected {} got {}", self.expected, self.found)
    }
}

fn value_kind(value: &OirValue) -> &'static str {
    match value {
        OirValue::Null => "null",
        OirValue::Bool(_) => "bool",
        OirValue::Number(_) => "number",
        OirValue::String(_) => "string",
        OirValue::Array(_) => "array",
        OirValue::Object(_) => "object",
    }
}

/// The `kind` tag `OirType` serializes under (`#[serde(tag = "kind", rename_all
/// = "lowercase")]`) — reused here so a mismatch names the type the same way
/// the `.orb` JSON does.
fn type_tag(ty: &OirType) -> String {
    match ty {
        OirType::String => "string".to_string(),
        OirType::Number => "number".to_string(),
        OirType::Integer => "integer".to_string(),
        OirType::Sized { width } => width.name().to_string(),
        OirType::Boolean => "boolean".to_string(),
        OirType::Date => "date".to_string(),
        OirType::DateTime => "datetime".to_string(),
        OirType::Instant => "instant".to_string(),
        OirType::Email => "email".to_string(),
        OirType::Url => "url".to_string(),
        OirType::Phone => "phone".to_string(),
        OirType::Uuid => "uuid".to_string(),
        OirType::Image => "image".to_string(),
        OirType::Bytes => "bytes".to_string(),
        OirType::CallKey => "callkey".to_string(),
        OirType::FieldRef { .. } => "fieldref".to_string(),
        OirType::Money => "money".to_string(),
        OirType::File => "file".to_string(),
        OirType::Node => "node".to_string(),
        OirType::Array { .. } => "array".to_string(),
        OirType::Tuple { .. } => "tuple".to_string(),
        OirType::Map { .. } => "map".to_string(),
        OirType::Optional { .. } => "optional".to_string(),
        OirType::Reference { entity } => format!("reference({entity})"),
        OirType::Row { entity } => format!("row({entity})"),
        OirType::Enum { name } => format!("enum({name})"),
        OirType::Struct { .. } => "struct".to_string(),
        OirType::Union { .. } => "union".to_string(),
        OirType::Trait => "trait".to_string(),
        OirType::Orbital => "orbital".to_string(),
        OirType::Behavior => "behavior".to_string(),
        OirType::SExpr => "sexpr".to_string(),
        OirType::TypeRef { name } => format!("typeref({name})"),
        OirType::Var { name } => format!("${name}"),
    }
}

/// Renders `value` (a field's `default`) against `ty` (the field's declared
/// type). `enum_name` is the field's `enum_values` sidecar name when `ty`
/// itself isn't an `OirType::Enum` (mirrors `render_type`'s field-position
/// sidecar). Returns `Ok(None)` when the field legitimately has no default to
/// emit (a bare `Null` against a non-optional type — the validator, not this
/// renderer, owns rejecting that); `Err` when `value`'s kind cannot possibly
/// inhabit `ty` at all.
pub fn render_default<R: TargetLiteralRenderer>(
    r: &R,
    value: &OirValue,
    ty: &OirType,
    enum_name: Option<&str>,
) -> Result<Option<R::Out>, LiteralMismatch> {
    match (ty, value) {
        (OirType::Optional { .. }, OirValue::Null) => Ok(Some(r.null())),
        (OirType::Optional { inner }, v) => render_default(r, v, inner, enum_name),

        (OirType::Enum { name }, OirValue::String(s)) => {
            Ok(Some(r.enum_member(enum_name.unwrap_or(name), s)))
        }

        // String-shaped leaves: every semantic string domain, plus Reference
        // (a relation field stores the referenced row's id — `render.rs`
        // C1) and File (a default names/points at the stored artifact, not
        // the full `{name,url,mimeType,sizeBytes}` struct).
        (OirType::String, OirValue::String(s))
        | (OirType::Reference { .. }, OirValue::String(s))
        | (OirType::Email, OirValue::String(s))
        | (OirType::Url, OirValue::String(s))
        | (OirType::Phone, OirValue::String(s))
        | (OirType::Uuid, OirValue::String(s))
        | (OirType::Date, OirValue::String(s))
        | (OirType::DateTime, OirValue::String(s))
        | (OirType::Instant, OirValue::String(s))
        | (OirType::Image, OirValue::String(s))
        | (OirType::Bytes, OirValue::String(s))
        | (OirType::CallKey, OirValue::String(s))
        | (OirType::FieldRef { .. }, OirValue::String(s))
        | (OirType::File, OirValue::String(s)) => Ok(Some(r.string(s))),

        (OirType::Number, OirValue::Number(n))
        | (OirType::Money, OirValue::Number(n)) => {
            Ok(Some(r.number(*n)))
        }
        (OirType::Integer, OirValue::Number(n)) if n.fract() == 0.0 => {
            Ok(Some(r.integer(*n as i64)))
        }
        (OirType::Sized { width }, OirValue::Number(n))
            if width.fits(&crate::value::Value::Number(*n)) =>
        {
            Ok(Some(if width.is_float() { r.number(*n) } else { r.integer(*n as i64) }))
        }
        (OirType::Boolean, OirValue::Bool(b)) => Ok(Some(r.bool(*b))),

        (OirType::Array { inner }, OirValue::Array(items)) => {
            let mut rendered = Vec::with_capacity(items.len());
            for item in items {
                match render_default(r, item, inner, None)? {
                    Some(out) => rendered.push(out),
                    None => continue, // a null element renders as no literal; skip it
                }
            }
            Ok(Some(r.array(rendered)))
        }

        (OirType::Node, OirValue::String(s)) => {
            r.node_text(s).map(Some).ok_or(LiteralMismatch { expected: type_tag(ty), found: value_kind(value) })
        }

        (OirType::Map { value: entry, .. }, OirValue::Object(fields)) => {
            let mut entries = Vec::with_capacity(fields.len());
            for (key, item) in fields {
                if let Some(out) = render_default(r, item, entry, None)? {
                    entries.push((key.clone(), out));
                }
            }
            Ok(r.map(entries).or_else(|| r.object()))
        }

        (
            OirType::Struct { .. }
            | OirType::Node
            | OirType::Row { .. },
            OirValue::Object(_),
        ) => Ok(r.object()),

        // A bare `Null` against a non-optional type: no default to emit. The
        // validator owns rejecting a `Null` default on a required field.
        (_, OirValue::Null) => Ok(None),

        // A union default renders at the first arm, in declaration order, whose type the value fits.
        (OirType::Union { variants }, v) => match variants
            .iter()
            .find_map(|arm| render_default(r, v, &arm.field_type, None).ok().map(|out| (arm, out)))
        {
            Some((arm, Some(out))) => Ok(Some(r.union_arm(&arm.name, out))),
            Some((_, None)) => Ok(None),
            None => Err(LiteralMismatch { expected: type_tag(ty), found: value_kind(value) }),
        },

        (expected, found) => Err(LiteralMismatch {
            expected: type_tag(expected),
            found: value_kind(found),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::OirField;

    struct StringRenderer;

    impl TargetLiteralRenderer for StringRenderer {
        type Out = String;

        fn null(&self) -> String {
            "null".to_string()
        }
        fn bool(&self, b: bool) -> String {
            b.to_string()
        }
        fn integer(&self, i: i64) -> String {
            i.to_string()
        }
        fn number(&self, n: f64) -> String {
            n.to_string()
        }
        fn string(&self, s: &str) -> String {
            format!("\"{s}\"")
        }
        fn enum_member(&self, enum_name: &str, member: &str) -> String {
            format!("{enum_name}.{member}")
        }
        fn array(&self, items: Vec<String>) -> String {
            format!("[{}]", items.join(","))
        }
    }

    struct ObjectRenderer;

    impl TargetLiteralRenderer for ObjectRenderer {
        type Out = String;

        fn null(&self) -> String {
            "null".to_string()
        }
        fn bool(&self, b: bool) -> String {
            b.to_string()
        }
        fn integer(&self, i: i64) -> String {
            i.to_string()
        }
        fn number(&self, n: f64) -> String {
            n.to_string()
        }
        fn string(&self, s: &str) -> String {
            format!("\"{s}\"")
        }
        fn enum_member(&self, enum_name: &str, member: &str) -> String {
            format!("{enum_name}.{member}")
        }
        fn array(&self, items: Vec<String>) -> String {
            format!("[{}]", items.join(","))
        }
        fn object(&self) -> Option<String> {
            Some("{}".to_string())
        }
    }

    fn r() -> StringRenderer {
        StringRenderer
    }

    struct TaggedRenderer;

    impl TargetLiteralRenderer for TaggedRenderer {
        type Out = String;
        fn null(&self) -> String {
            "null".to_string()
        }
        fn bool(&self, b: bool) -> String {
            b.to_string()
        }
        fn integer(&self, i: i64) -> String {
            i.to_string()
        }
        fn number(&self, n: f64) -> String {
            n.to_string()
        }
        fn string(&self, s: &str) -> String {
            format!("\"{s}\"")
        }
        fn enum_member(&self, enum_name: &str, member: &str) -> String {
            format!("{enum_name}.{member}")
        }
        fn array(&self, items: Vec<String>) -> String {
            format!("[{}]", items.join(","))
        }
        fn union_arm(&self, arm: &str, inner: String) -> String {
            format!("{arm}({inner})")
        }
    }

    fn host_union() -> OirType {
        OirType::Union {
            variants: vec![
                OirField::new("string", OirType::String),
                OirField::new("Env", OirType::Struct { fields: vec![OirField::new("anchor", OirType::String)].into(), additional: None }),
            ]
            .into(),
        }
    }

    #[test]
    fn a_union_default_renders_at_the_first_arm_its_value_fits() {
        let host = OirValue::String("api.example.test".to_string());
        assert_eq!(render_default(&r(), &host, &host_union(), None), Ok(Some("\"api.example.test\"".to_string())));
        assert_eq!(render_default(&TaggedRenderer, &host, &host_union(), None), Ok(Some("string(\"api.example.test\")".to_string())));
        assert_eq!(
            render_default(&TaggedRenderer, &OirValue::Number(1.0), &host_union(), None),
            Err(LiteralMismatch { expected: "union".to_string(), found: "number" }),
            "control: a value no arm fits is a mismatch"
        );
        assert_eq!(render_default(&TaggedRenderer, &OirValue::Null, &host_union(), None), Ok(None), "control: null has no default");
    }

    struct NodeTextRenderer;

    impl TargetLiteralRenderer for NodeTextRenderer {
        type Out = String;
        fn null(&self) -> String {
            "null".to_string()
        }
        fn bool(&self, b: bool) -> String {
            b.to_string()
        }
        fn integer(&self, i: i64) -> String {
            i.to_string()
        }
        fn number(&self, n: f64) -> String {
            n.to_string()
        }
        fn string(&self, s: &str) -> String {
            format!("\"{s}\"")
        }
        fn enum_member(&self, enum_name: &str, member: &str) -> String {
            format!("{enum_name}.{member}")
        }
        fn array(&self, items: Vec<String>) -> String {
            format!("[{}]", items.join(", "))
        }
        fn node_text(&self, s: &str) -> Option<String> {
            Some(format!("Node.text(\"{s}\")"))
        }
        fn map(&self, entries: Vec<(String, String)>) -> Option<String> {
            Some(format!("{{{}}}", entries.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join(", ")))
        }
    }

    #[test]
    fn a_map_default_renders_its_entries_through_the_shells_map() {
        let ty = OirType::Map { key: Box::new(OirType::String), value: Box::new(OirType::Array { inner: Box::new(OirType::Number) }) };
        let mut fields = crate::ir::types::OirObject::default();
        fields.insert("open".to_string(), OirValue::Array(vec![OirValue::Number(1.0)]));
        fields.insert("closed".to_string(), OirValue::Array(vec![]));
        assert_eq!(render_default(&NodeTextRenderer, &OirValue::Object(fields.clone()), &ty, None), Ok(Some("{open: [1], closed: []}".to_string())));
        assert_eq!(render_default(&NodeTextRenderer, &OirValue::Object(Default::default()), &ty, None), Ok(Some("{}".to_string())));
        assert_eq!(render_default(&ObjectRenderer, &OirValue::Object(fields.clone()), &ty, None), Ok(Some("{}".to_string())), "control: a shell without map keeps its object default");
        assert_eq!(render_default(&r(), &OirValue::Object(fields), &ty, None), Ok(None), "control: a shell with neither emits no default");
        let mut bad = crate::ir::types::OirObject::default();
        bad.insert("open".to_string(), OirValue::String("x".to_string()));
        assert!(render_default(&NodeTextRenderer, &OirValue::Object(bad), &ty, None).is_err(), "control: an entry that does not fit the value type is refused");
    }

    #[test]
    fn a_text_default_of_a_node_renders_through_the_shells_node_text() {
        let text = OirValue::String("Ticket details".to_string());
        assert_eq!(render_default(&NodeTextRenderer, &text, &OirType::Node, None), Ok(Some("Node.text(\"Ticket details\")".to_string())));
        assert_eq!(
            render_default(&r(), &text, &OirType::Node, None),
            Err(LiteralMismatch { expected: "node".to_string(), found: "string" }),
            "control: a shell without node_text still refuses it"
        );
        assert_eq!(render_default(&NodeTextRenderer, &OirValue::Number(1.0), &OirType::Node, None).map(|_| ()), Err(LiteralMismatch { expected: "node".to_string(), found: "number" }), "control: a number is not a text node");
    }

    #[test]
    fn null_against_optional_renders_null() {
        assert_eq!(
            render_default(&r(), &OirValue::Null, &OirType::optional(OirType::String), None),
            Ok(Some("null".to_string()))
        );
    }

    #[test]
    fn value_against_optional_recurses_into_inner() {
        assert_eq!(
            render_default(
                &r(),
                &OirValue::Number(1.0),
                &OirType::optional(OirType::Integer),
                None
            ),
            Ok(Some("1".to_string()))
        );
    }

    #[test]
    fn enum_arm_uses_enum_name_override_when_given() {
        assert_eq!(
            render_default(
                &r(),
                &OirValue::String("active".to_string()),
                &OirType::enum_type("Status"),
                Some("OverrideName")
            ),
            Ok(Some("OverrideName.active".to_string()))
        );
        assert_eq!(
            render_default(
                &r(),
                &OirValue::String("active".to_string()),
                &OirType::enum_type("Status"),
                None
            ),
            Ok(Some("Status.active".to_string()))
        );
    }

    #[test]
    fn string_shaped_leaves_render_via_string() {
        for ty in [
            OirType::String,
            OirType::reference("User"),
            OirType::Email,
            OirType::Url,
            OirType::Phone,
            OirType::Uuid,
            OirType::Date,
            OirType::DateTime,
            OirType::Image,
            OirType::Bytes,
            OirType::File,
        ] {
            assert_eq!(
                render_default(&r(), &OirValue::String("x".to_string()), &ty, None),
                Ok(Some("\"x\"".to_string())),
                "{ty:?}"
            );
        }
    }

    #[test]
    fn number_and_money_render_via_number() {
        assert_eq!(
            render_default(&r(), &OirValue::Number(1.5), &OirType::Number, None),
            Ok(Some("1.5".to_string()))
        );
        assert_eq!(
            render_default(&r(), &OirValue::Number(1.5), &OirType::Money, None),
            Ok(Some("1.5".to_string()))
        );
    }

    #[test]
    fn integer_requires_a_whole_number() {
        assert_eq!(
            render_default(&r(), &OirValue::Number(3.0), &OirType::Integer, None),
            Ok(Some("3".to_string()))
        );
        assert!(render_default(&r(), &OirValue::Number(3.5), &OirType::Integer, None).is_err());
    }

    #[test]
    fn boolean_renders_via_bool() {
        assert_eq!(
            render_default(&r(), &OirValue::Bool(true), &OirType::Boolean, None),
            Ok(Some("true".to_string()))
        );
    }

    #[test]
    fn array_recurses_element_by_element() {
        let value = OirValue::Array(vec![OirValue::Number(1.0), OirValue::Number(2.0)]);
        assert_eq!(
            render_default(&r(), &value, &OirType::array(OirType::Integer), None),
            Ok(Some("[1,2]".to_string()))
        );
    }

    #[test]
    fn array_propagates_the_first_element_mismatch() {
        let value = OirValue::Array(vec![OirValue::Bool(true)]);
        let err = render_default(&r(), &value, &OirType::array(OirType::Integer), None)
            .expect_err("bool element against Integer array");
        assert_eq!(err.found, "bool");
        assert_eq!(err.expected, "integer");
    }

    #[test]
    fn struct_map_node_scalar_defer_to_renderer_object() {
        let mut fields = crate::ir::types::OirObject::default();
        fields.insert("a".to_string(), OirValue::Number(1.0));
        let value = OirValue::Object(fields);
        for ty in [
            OirType::Struct { fields: crate::ir::types::no_fields(), additional: None },
            OirType::map(OirType::String, OirType::Number),
            OirType::Node,
        ] {
            // The default `TargetLiteralRenderer::object` declines.
            assert_eq!(render_default(&r(), &value, &ty, None), Ok(None), "{ty:?}");
            // A renderer that supplies one gets it back.
            assert_eq!(
                render_default(&ObjectRenderer, &value, &ty, None),
                Ok(Some("{}".to_string())),
                "{ty:?}"
            );
        }
    }

    #[test]
    fn empty_array_default_is_accepted_for_array_of_row() {
        let ty = OirType::array(OirType::row("Revision"));
        assert_eq!(
            render_default(&r(), &OirValue::Array(vec![]), &ty, None),
            Ok(Some("[]".to_string()))
        );
    }

    #[test]
    fn row_object_is_accepted_and_defers_to_renderer_object() {
        let mut fields = crate::ir::types::OirObject::default();
        fields.insert("a".to_string(), OirValue::Number(1.0));
        let value = OirValue::Object(fields);
        let ty = OirType::row("Revision");
        assert_eq!(render_default(&r(), &value, &ty, None), Ok(None));
        assert_eq!(
            render_default(&ObjectRenderer, &value, &ty, None),
            Ok(Some("{}".to_string()))
        );
    }

    #[test]
    fn row_rejects_an_id_string_where_reference_accepts_it() {
        let id = OirValue::String("r1".to_string());
        assert_eq!(
            render_default(&r(), &id, &OirType::reference("Revision"), None),
            Ok(Some("\"r1\"".to_string()))
        );
        let err = render_default(&r(), &id, &OirType::row("Revision"), None)
            .expect_err("string against Row");
        assert_eq!(err.expected, "row(Revision)");
        assert_eq!(err.found, "string");
    }

    #[test]
    fn null_against_a_non_optional_type_emits_no_default() {
        assert_eq!(
            render_default(&r(), &OirValue::Null, &OirType::String, None),
            Ok(None)
        );
        assert_eq!(
            render_default(&r(), &OirValue::Null, &OirType::Boolean, None),
            Ok(None)
        );
    }

    #[test]
    fn mismatches_are_rejected_not_silently_accepted() {
        let boolean_vs_string = render_default(
            &r(),
            &OirValue::String("x".to_string()),
            &OirType::Boolean,
            None,
        )
        .expect_err("String value against Boolean type");
        assert_eq!(boolean_vs_string.expected, "boolean");
        assert_eq!(boolean_vs_string.found, "string");

        let mut fields = crate::ir::types::OirObject::default();
        fields.insert("a".to_string(), OirValue::Number(1.0));
        let reference_vs_object = render_default(
            &r(),
            &OirValue::Object(fields),
            &OirType::reference("User"),
            None,
        )
        .expect_err("Object value against Reference type");
        assert_eq!(reference_vs_object.expected, "reference(User)");
        assert_eq!(reference_vs_object.found, "object");

        let number_vs_string = render_default(
            &r(),
            &OirValue::String("x".to_string()),
            &OirType::Number,
            None,
        )
        .expect_err("String value against Number type");
        assert_eq!(number_vs_string.expected, "number");
        assert_eq!(number_vs_string.found, "string");

        let enum_vs_number = render_default(&r(), &OirValue::Number(1.0), &OirType::enum_type("Status"), None)
            .expect_err("Number value against Enum type");
        assert_eq!(enum_vs_number.expected, "enum(Status)");
        assert_eq!(enum_vs_number.found, "number");
    }
}
