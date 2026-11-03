//! JSON for a JavaScript reader (EXEC-EVAL-11): `JSON.parse` reads every number as a float, so an integer beyond 2^53
//! is refused here, never written for the reader to round.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::value::MAX_EXACT_INT;
use serde::ser::{self, Serialize};

/// `value` as JSON text a JavaScript host reads exactly, or the path and value of the first integer it cannot.
pub fn to_js_readable_json<T: Serialize + ?Sized>(value: &T) -> Result<String, String> {
    js_readable(value).map_err(|e| e.to_string())?;
    serde_json::to_string(value).map_err(|e| format!("serialize: {e}"))
}

/// Whether every integer in `value` is one a JavaScript host reads exactly.
pub fn js_readable<T: Serialize + ?Sized>(value: &T) -> Result<(), BeyondExact> {
    value.serialize(Check)
}

#[derive(Debug)]
pub struct BeyondExact {
    number: String,
    /// Innermost segment first.
    path: Vec<String>,
}

impl BeyondExact {
    fn within(mut self, segment: impl Into<String>) -> Self {
        self.path.push(segment.into());
        self
    }
}

impl core::fmt::Display for BeyondExact {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let at: Vec<&str> = self.path.iter().rev().map(String::as_str).collect();
        let at = if at.is_empty() { "the value".to_string() } else { at.join(".") };
        write!(f, "{at} is the int {}, beyond 2^53, which a JavaScript host cannot read exactly", self.number)
    }
}

impl ser::StdError for BeyondExact {}

impl ser::Error for BeyondExact {
    fn custom<M: core::fmt::Display>(msg: M) -> Self {
        BeyondExact { number: msg.to_string(), path: Vec::new() }
    }
}

fn beyond(number: impl ToString) -> BeyondExact {
    BeyondExact { number: number.to_string(), path: Vec::new() }
}

fn key_text<K: Serialize + ?Sized>(key: &K) -> String {
    match serde_json::to_value(key) {
        Ok(serde_json::Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(_) => "?".to_string(),
    }
}

#[derive(Clone, Copy)]
struct Check;

type Checked = Result<(), BeyondExact>;

impl ser::Serializer for Check {
    type Ok = ();
    type Error = BeyondExact;
    type SerializeSeq = Items;
    type SerializeTuple = Items;
    type SerializeTupleStruct = Items;
    type SerializeTupleVariant = Items;
    type SerializeMap = Entries;
    type SerializeStruct = Check;
    type SerializeStructVariant = Check;

    fn serialize_i64(self, v: i64) -> Checked {
        if v.unsigned_abs() <= MAX_EXACT_INT as u64 { Ok(()) } else { Err(beyond(v)) }
    }
    fn serialize_u64(self, v: u64) -> Checked {
        if v <= MAX_EXACT_INT as u64 { Ok(()) } else { Err(beyond(v)) }
    }
    fn serialize_i128(self, v: i128) -> Checked {
        if v.unsigned_abs() <= MAX_EXACT_INT as u128 { Ok(()) } else { Err(beyond(v)) }
    }
    fn serialize_u128(self, v: u128) -> Checked {
        if v <= MAX_EXACT_INT as u128 { Ok(()) } else { Err(beyond(v)) }
    }
    fn serialize_bool(self, _: bool) -> Checked {
        Ok(())
    }
    fn serialize_i8(self, _: i8) -> Checked {
        Ok(())
    }
    fn serialize_i16(self, _: i16) -> Checked {
        Ok(())
    }
    fn serialize_i32(self, _: i32) -> Checked {
        Ok(())
    }
    fn serialize_u8(self, _: u8) -> Checked {
        Ok(())
    }
    fn serialize_u16(self, _: u16) -> Checked {
        Ok(())
    }
    fn serialize_u32(self, _: u32) -> Checked {
        Ok(())
    }
    fn serialize_f32(self, _: f32) -> Checked {
        Ok(())
    }
    fn serialize_f64(self, _: f64) -> Checked {
        Ok(())
    }
    fn serialize_char(self, _: char) -> Checked {
        Ok(())
    }
    fn serialize_str(self, _: &str) -> Checked {
        Ok(())
    }
    fn serialize_bytes(self, _: &[u8]) -> Checked {
        Ok(())
    }
    fn serialize_none(self) -> Checked {
        Ok(())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Checked {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Checked {
        Ok(())
    }
    fn serialize_unit_struct(self, _: &'static str) -> Checked {
        Ok(())
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Checked {
        Ok(())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _: &'static str, value: &T) -> Checked {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(self, _: &'static str, _: u32, variant: &'static str, value: &T) -> Checked {
        value.serialize(self).map_err(|e| e.within(variant))
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Items, BeyondExact> {
        Ok(Items(0))
    }
    fn serialize_tuple(self, _: usize) -> Result<Items, BeyondExact> {
        Ok(Items(0))
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Items, BeyondExact> {
        Ok(Items(0))
    }
    fn serialize_tuple_variant(self, _: &'static str, _: u32, _: &'static str, _: usize) -> Result<Items, BeyondExact> {
        Ok(Items(0))
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Entries, BeyondExact> {
        Ok(Entries)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Check, BeyondExact> {
        Ok(Check)
    }
    fn serialize_struct_variant(self, _: &'static str, _: u32, _: &'static str, _: usize) -> Result<Check, BeyondExact> {
        Ok(Check)
    }
}

struct Items(usize);

impl Items {
    fn item<T: Serialize + ?Sized>(&mut self, value: &T) -> Checked {
        let at = self.0;
        self.0 += 1;
        value.serialize(Check).map_err(|e| e.within(at.to_string()))
    }
}

impl ser::SerializeSeq for Items {
    type Ok = ();
    type Error = BeyondExact;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Checked {
        self.item(value)
    }
    fn end(self) -> Checked {
        Ok(())
    }
}

impl ser::SerializeTuple for Items {
    type Ok = ();
    type Error = BeyondExact;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Checked {
        self.item(value)
    }
    fn end(self) -> Checked {
        Ok(())
    }
}

impl ser::SerializeTupleStruct for Items {
    type Ok = ();
    type Error = BeyondExact;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Checked {
        self.item(value)
    }
    fn end(self) -> Checked {
        Ok(())
    }
}

impl ser::SerializeTupleVariant for Items {
    type Ok = ();
    type Error = BeyondExact;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Checked {
        self.item(value)
    }
    fn end(self) -> Checked {
        Ok(())
    }
}

struct Entries;

impl ser::SerializeMap for Entries {
    type Ok = ();
    type Error = BeyondExact;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Checked {
        key.serialize(Check)
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Checked {
        value.serialize(Check)
    }
    fn serialize_entry<K: Serialize + ?Sized, V: Serialize + ?Sized>(&mut self, key: &K, value: &V) -> Checked {
        key.serialize(Check)?;
        value.serialize(Check).map_err(|e| e.within(key_text(key)))
    }
    fn end(self) -> Checked {
        Ok(())
    }
}

impl ser::SerializeStruct for Check {
    type Ok = ();
    type Error = BeyondExact;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Checked {
        value.serialize(Check).map_err(|e| e.within(key))
    }
    fn end(self) -> Checked {
        Ok(())
    }
}

impl ser::SerializeStructVariant for Check {
    type Ok = ();
    type Error = BeyondExact;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Checked {
        value.serialize(Check).map_err(|e| e.within(key))
    }
    fn end(self) -> Checked {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;
    use serde_json::json;

    const BEYOND: i64 = MAX_EXACT_INT + 1;

    #[test]
    fn an_int_beyond_2_53_is_refused_with_its_path_and_one_at_2_53_is_written() {
        let refused = to_js_readable_json(&json!({ "response": { "rows": [{ "n": 1 }, { "n": BEYOND }] } })).unwrap_err();
        assert_eq!(refused, "response.rows.1.n is the int 9007199254740993, beyond 2^53, which a JavaScript host cannot read exactly");
        assert_eq!(to_js_readable_json(&json!({ "n": MAX_EXACT_INT })).unwrap(), "{\"n\":9007199254740992}", "control: 2^53 is exact");
        assert!(to_js_readable_json(&json!(-BEYOND)).is_err(), "a negative beyond 2^53 too");
    }

    #[test]
    fn a_kernel_int_and_an_unsigned_wire_counter_are_both_checked() {
        let mut row = crate::value::ValueMap::default();
        row.insert("id".into(), Value::Int(BEYOND));
        assert!(to_js_readable_json(&Value::Object(row.into())).unwrap_err().starts_with("id is the int"));
        assert!(to_js_readable_json(&(u64::MAX,)).unwrap_err().starts_with("0 is the int 18446744073709551615"));
    }

    #[test]
    fn control_floats_and_text_pass_whatever_their_size() {
        assert!(to_js_readable_json(&json!({ "big": 1e300, "digits": "9007199254740993" })).is_ok());
        assert!(to_js_readable_json(&Value::Number(9007199254740994.0)).is_ok(), "a float is already what JavaScript reads");
    }
}
