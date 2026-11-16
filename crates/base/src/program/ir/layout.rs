//! Machine layouts (P-039): how compiled code holds a value of each `OirType`, where each field of a record sits,
//! and what a binding path reads. A type whose run-time kind is not fixed by its declaration is [`Repr::Boxed`],
//! the kernel's dynamic `Value`. Sizes and alignment belong to the code generator's target, not here.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::{Deserialize, Serialize};

use crate::ir::expr::OirBinding;
use crate::ir::types::{OirField, OirType};
use crate::num_width::NumWidth;

/// How compiled code holds a value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "repr", rename_all = "kebab-case")]
pub enum Repr {
    Bool,
    /// An exact integer of the width; `int` is `i64`.
    Int { width: NumWidth },
    /// `number`: an exact int or a float, tagged, because an int widens to a number without changing (EXEC-EVAL-11).
    Number,
    /// A shared string: `string` and every type that is a string at run time.
    Text,
    /// Shared bytes.
    Bytes,
    /// A shared list of one element representation.
    List { of: Box<Repr> },
    /// A fixed-length list with a representation per slot.
    Tuple { items: Vec<Repr> },
    /// A record whose fields sit in declared order.
    Record { layout: RecordLayout },
    /// A shared map with keys known only at run time.
    Map { value: Box<Repr> },
    /// `inner` or null, tagged.
    Optional { inner: Box<Repr> },
    /// One of the named variants, tagged, in declaration order.
    Union { variants: Vec<FieldSlot> },
    /// A row of the named entity: its layout is the entity's.
    Row { entity: String },
    /// A named type held by its name, so a recursive type stays finite.
    Named { name: String },
    /// The dynamic `Value`.
    Boxed,
}

/// One field of a record, at its slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldSlot {
    pub name: String,
    pub repr: Repr,
    /// A field that is not required may be absent, and reads as null.
    pub required: bool,
}

/// The fields of a record in declared order: a field's slot is its index.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RecordLayout {
    pub fields: Vec<FieldSlot>,
}

impl RecordLayout {
    pub fn of(fields: &[OirField]) -> Self {
        RecordLayout { fields: fields.iter().map(slot_of).collect() }
    }

    /// A record declared as name-to-type pairs (an event payload), every field required.
    pub fn of_types<'a>(types: impl IntoIterator<Item = (&'a String, &'a OirType)>) -> Self {
        RecordLayout {
            fields: types
                .into_iter()
                .map(|(name, ty)| FieldSlot { name: name.clone(), repr: Repr::of(ty), required: true })
                .collect(),
        }
    }

    pub fn slot(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|field| field.name == name)
    }
}

fn slot_of(field: &OirField) -> FieldSlot {
    FieldSlot { name: field.name.clone(), repr: Repr::of(&field.field_type), required: field.required }
}

impl Repr {
    /// The representation `ty` declares.
    pub fn of(ty: &OirType) -> Repr {
        match ty {
            OirType::Boolean => Repr::Bool,
            OirType::Integer => Repr::Int { width: NumWidth::I64 },
            OirType::Sized { width } if !width.is_float() => Repr::Int { width: *width },
            OirType::Sized { .. } | OirType::Number | OirType::Money | OirType::Instant => Repr::Number,
            OirType::String
            | OirType::Email
            | OirType::Url
            | OirType::Phone
            | OirType::Uuid
            | OirType::Image
            | OirType::CallKey
            | OirType::MessageKey
            | OirType::FieldRef { .. }
            | OirType::Enum { .. }
            | OirType::Reference { .. }
            | OirType::Trait => Repr::Text,
            OirType::Bytes => Repr::Bytes,
            OirType::Array { inner } => Repr::List { of: Box::new(Repr::of(inner)) },
            OirType::Tuple { items } => Repr::Tuple { items: items.iter().map(Repr::of).collect() },
            OirType::Map { value, .. } => Repr::Map { value: Box::new(Repr::of(value)) },
            OirType::Optional { inner } => Repr::Optional { inner: Box::new(Repr::of(inner)) },
            OirType::Struct { fields, additional: None } => Repr::Record { layout: RecordLayout::of(fields) },
            OirType::Union { variants } => Repr::Union { variants: variants.iter().map(slot_of).collect() },
            OirType::Row { entity } => Repr::Row { entity: entity.clone() },
            OirType::TypeRef { name } => Repr::Named { name: name.clone() },
            OirType::Struct { additional: Some(_), .. }
            | OirType::Date
            | OirType::DateTime
            | OirType::File
            | OirType::Node
            | OirType::Orbital
            | OirType::Behavior
            | OirType::SExpr
            | OirType::Var { .. } => Repr::Boxed,
        }
    }
}

/// One step of a binding path, resolved against a layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "kebab-case")]
pub enum PathStep {
    /// The field at this slot of a record.
    Slot { slot: usize },
    /// The element at this index of a list or tuple.
    Index { index: usize },
    /// The number of elements of a list (`length`, `count`).
    Length,
    /// A key looked up at run time, in a map or a dynamic value.
    Key { key: String },
}

/// A binding path resolved against the layout of its root: the steps to take and the representation read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedPath {
    pub steps: Vec<PathStep>,
    pub repr: Repr,
}

fn optional(repr: Repr) -> Repr {
    match repr {
        already @ (Repr::Optional { .. } | Repr::Boxed) => already,
        inner => Repr::Optional { inner: Box::new(inner) },
    }
}

/// `binding`'s path against `root`, the representation of the value its root names. `row` gives an entity's record
/// layout and `named` a named type's representation; a name neither knows is read as a dynamic value. A step through
/// a value that may be null reads null, so everything after it is optional.
pub fn resolve_path(
    binding: &OirBinding,
    root: &Repr,
    row: &dyn Fn(&str) -> Option<RecordLayout>,
    named: &dyn Fn(&str) -> Option<Repr>,
) -> ResolvedPath {
    let mut steps = Vec::with_capacity(binding.path.len());
    let mut at = root.clone();
    let mut nullable = false;
    for segment in &binding.path {
        let mut seen = 0usize;
        loop {
            at = match at {
                Repr::Optional { inner } => {
                    nullable = true;
                    *inner
                }
                Repr::Row { entity } => match row(&entity) {
                    Some(layout) => Repr::Record { layout },
                    None => Repr::Boxed,
                },
                Repr::Named { name } if seen < 64 => {
                    seen += 1;
                    named(&name).unwrap_or(Repr::Boxed)
                }
                Repr::Named { .. } => Repr::Boxed,
                settled => {
                    at = settled;
                    break;
                }
            };
        }
        let index = segment.parse::<usize>().ok();
        let (step, next) = match (&at, index) {
            (Repr::Record { layout }, _) => match layout.slot(segment) {
                Some(slot) => {
                    let field = &layout.fields[slot];
                    nullable |= !field.required;
                    (PathStep::Slot { slot }, field.repr.clone())
                }
                None => (PathStep::Key { key: segment.clone() }, Repr::Boxed),
            },
            (Repr::Tuple { items }, Some(index)) if index < items.len() => (PathStep::Index { index }, items[index].clone()),
            (Repr::List { of }, Some(index)) => {
                nullable = true;
                (PathStep::Index { index }, (**of).clone())
            }
            (Repr::List { .. }, None) if segment == "length" || segment == "count" => {
                (PathStep::Length, Repr::Int { width: NumWidth::I64 })
            }
            (Repr::Map { value }, _) => {
                nullable = true;
                (PathStep::Key { key: segment.clone() }, (**value).clone())
            }
            _ => (PathStep::Key { key: segment.clone() }, Repr::Boxed),
        };
        steps.push(step);
        at = next;
    }
    ResolvedPath { steps, repr: if nullable { optional(at) } else { at } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::expr::OirBindingRoot;
    use crate::value::Shared;

    fn field(name: &str, ty: OirType, required: bool) -> OirField {
        let source = serde_json::json!({ "name": name, "fieldType": ty, "required": required });
        crate::json_value::from_json_value(&source).expect("a field")
    }

    fn record(fields: Vec<OirField>) -> OirType {
        OirType::Struct { fields: Shared::new(fields), additional: None }
    }

    fn path(segments: &[&str]) -> OirBinding {
        OirBinding::new(OirBindingRoot::Entity, segments.iter().map(|s| s.to_string()).collect())
    }

    fn resolve(binding: &OirBinding, root: &Repr) -> ResolvedPath {
        resolve_path(binding, root, &|_| None, &|_| None)
    }

    fn account() -> Repr {
        Repr::of(&record(vec![
            field("id", OirType::String, true),
            field("balance", OirType::Integer, true),
            field("ratio", OirType::Number, true),
            field("note", OirType::String, false),
            field("tags", OirType::Array { inner: Box::new(OirType::String) }, true),
            field("owner", record(vec![field("age", OirType::Sized { width: NumWidth::U8 }, true)]), true),
        ]))
    }

    #[test]
    fn a_record_field_sits_at_its_declared_slot() {
        let read = resolve(&path(&["balance"]), &account());
        assert_eq!(read.steps, vec![PathStep::Slot { slot: 1 }]);
        assert_eq!(read.repr, Repr::Int { width: NumWidth::I64 });
    }

    #[test]
    fn a_nested_field_keeps_its_width() {
        let read = resolve(&path(&["owner", "age"]), &account());
        assert_eq!(read.steps, vec![PathStep::Slot { slot: 5 }, PathStep::Slot { slot: 0 }]);
        assert_eq!(read.repr, Repr::Int { width: NumWidth::U8 });
    }

    #[test]
    fn a_number_is_not_a_bare_float() {
        assert_eq!(resolve(&path(&["ratio"]), &account()).repr, Repr::Number);
    }

    #[test]
    fn a_field_that_may_be_absent_reads_as_optional() {
        let read = resolve(&path(&["note"]), &account());
        assert_eq!(read.repr, Repr::Optional { inner: Box::new(Repr::Text) });
    }

    #[test]
    fn control_a_required_field_is_not_optional() {
        assert_eq!(resolve(&path(&["id"]), &account()).repr, Repr::Text);
    }

    #[test]
    fn a_list_element_may_be_out_of_range_and_its_length_is_an_int() {
        let element = resolve(&path(&["tags", "0"]), &account());
        assert_eq!(element.steps, vec![PathStep::Slot { slot: 4 }, PathStep::Index { index: 0 }]);
        assert_eq!(element.repr, Repr::Optional { inner: Box::new(Repr::Text) });
        let length = resolve(&path(&["tags", "length"]), &account());
        assert_eq!(length.steps, vec![PathStep::Slot { slot: 4 }, PathStep::Length]);
        assert_eq!(length.repr, Repr::Int { width: NumWidth::I64 });
    }

    #[test]
    fn a_field_the_record_does_not_declare_is_a_dynamic_key() {
        let read = resolve(&path(&["missing", "deeper"]), &account());
        assert_eq!(
            read.steps,
            vec![PathStep::Key { key: "missing".to_string() }, PathStep::Key { key: "deeper".to_string() }]
        );
        assert_eq!(read.repr, Repr::Boxed);
    }

    #[test]
    fn a_row_reads_through_the_entity_layout_and_an_unknown_entity_is_dynamic() {
        let root = Repr::Row { entity: "Account".to_string() };
        let Repr::Record { layout } = account() else { panic!("a record") };
        let known = resolve_path(&path(&["balance"]), &root, &|name| (name == "Account").then(|| layout.clone()), &|_| None);
        assert_eq!(known.steps, vec![PathStep::Slot { slot: 1 }]);
        let unknown = resolve(&path(&["balance"]), &root);
        assert_eq!(unknown.steps, vec![PathStep::Key { key: "balance".to_string() }]);
        assert_eq!(unknown.repr, Repr::Boxed);
    }

    #[test]
    fn a_recursive_named_type_stays_finite() {
        let root = Repr::Named { name: "Loop".to_string() };
        let read = resolve_path(&path(&["x"]), &root, &|_| None, &|_| Some(Repr::Named { name: "Loop".to_string() }));
        assert_eq!(read.repr, Repr::Boxed);
    }

    #[test]
    fn a_type_with_no_fixed_run_time_kind_is_boxed() {
        for ty in [OirType::Date, OirType::DateTime, OirType::File, OirType::Node, OirType::SExpr] {
            assert_eq!(Repr::of(&ty), Repr::Boxed, "{ty:?}");
        }
    }
}
