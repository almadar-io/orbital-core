//! A program's `serde::Serialize` form built straight into a core [`Value`], with no `serde_json::Value` tree in
//! between: text is shared through the active intern scope, and `canonical` sorts every object's keys as
//! `into_canonical_json` does. Non-finite numbers are `null`, as `serde_json::to_value` has them.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::value::{Shared, Value, ValueMap};
use serde::ser::{self, Serialize};
use std::fmt;

#[derive(Debug)]
pub struct ValueSerializeError(String);

impl fmt::Display for ValueSerializeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ValueSerializeError {}

impl ser::Error for ValueSerializeError {
    fn custom<T: fmt::Display>(message: T) -> Self {
        ValueSerializeError(message.to_string())
    }
}

/// `value` as a core `Value`, objects in the order they are serialized.
pub fn to_value<T: Serialize + ?Sized>(value: &T) -> Result<Value, ValueSerializeError> {
    crate::intern::scope(|| value.serialize(ValueSerializer { canonical: false }))
}

/// [`to_value`] with every object's keys sorted.
pub fn to_canonical_value<T: Serialize + ?Sized>(value: &T) -> Result<Value, ValueSerializeError> {
    crate::intern::scope(|| value.serialize(ValueSerializer { canonical: true }))
}

#[derive(Clone, Copy)]
struct ValueSerializer {
    canonical: bool,
}

fn text(value: &str) -> Value {
    Value::String(crate::intern::shared(value))
}

fn object(mut fields: ValueMap, canonical: bool) -> Value {
    if canonical {
        fields.sort_keys();
    }
    crate::intern::value_record(fields)
}

pub struct SeqBuilder {
    items: Vec<Value>,
    canonical: bool,
    variant: Option<&'static str>,
}

impl SeqBuilder {
    fn finish(self) -> Value {
        let array = crate::intern::value_list(self.items);
        match self.variant {
            None => array,
            Some(name) => {
                let mut fields = ValueMap::default();
                fields.insert(name.to_owned().into(), array);
                object(fields, self.canonical)
            }
        }
    }

    fn push<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), ValueSerializeError> {
        self.items.push(value.serialize(ValueSerializer {
            canonical: self.canonical,
        })?);
        Ok(())
    }
}

impl ser::SerializeSeq for SeqBuilder {
    type Ok = Value;
    type Error = ValueSerializeError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.push(value)
    }
    fn end(self) -> Result<Value, Self::Error> {
        Ok(self.finish())
    }
}

impl ser::SerializeTuple for SeqBuilder {
    type Ok = Value;
    type Error = ValueSerializeError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.push(value)
    }
    fn end(self) -> Result<Value, Self::Error> {
        Ok(self.finish())
    }
}

impl ser::SerializeTupleStruct for SeqBuilder {
    type Ok = Value;
    type Error = ValueSerializeError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.push(value)
    }
    fn end(self) -> Result<Value, Self::Error> {
        Ok(self.finish())
    }
}

impl ser::SerializeTupleVariant for SeqBuilder {
    type Ok = Value;
    type Error = ValueSerializeError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.push(value)
    }
    fn end(self) -> Result<Value, Self::Error> {
        Ok(self.finish())
    }
}

pub struct MapBuilder {
    fields: ValueMap,
    pending: Option<String>,
    canonical: bool,
    variant: Option<&'static str>,
}

impl MapBuilder {
    fn finish(self) -> Value {
        let inner = object(self.fields, self.canonical);
        match self.variant {
            None => inner,
            Some(name) => {
                let mut fields = ValueMap::default();
                fields.insert(name.to_owned().into(), inner);
                object(fields, self.canonical)
            }
        }
    }

    fn field<T: Serialize + ?Sized>(
        &mut self,
        key: &str,
        value: &T,
    ) -> Result<(), ValueSerializeError> {
        let value = value.serialize(ValueSerializer {
            canonical: self.canonical,
        })?;
        self.fields.insert(key.to_owned().into(), value);
        Ok(())
    }
}

impl ser::SerializeMap for MapBuilder {
    type Ok = Value;
    type Error = ValueSerializeError;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Self::Error> {
        self.pending = Some(key.serialize(KeySerializer)?);
        Ok(())
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        let key = self
            .pending
            .take()
            .ok_or_else(|| ser::Error::custom("a map value was written before its key"))?;
        let value = value.serialize(ValueSerializer {
            canonical: self.canonical,
        })?;
        self.fields.insert(key.into(), value);
        Ok(())
    }
    fn end(self) -> Result<Value, Self::Error> {
        Ok(self.finish())
    }
}

impl ser::SerializeStruct for MapBuilder {
    type Ok = Value;
    type Error = ValueSerializeError;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.field(key, value)
    }
    fn end(self) -> Result<Value, Self::Error> {
        Ok(self.finish())
    }
}

impl ser::SerializeStructVariant for MapBuilder {
    type Ok = Value;
    type Error = ValueSerializeError;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.field(key, value)
    }
    fn end(self) -> Result<Value, Self::Error> {
        Ok(self.finish())
    }
}

impl ser::Serializer for ValueSerializer {
    type Ok = Value;
    type Error = ValueSerializeError;
    type SerializeSeq = SeqBuilder;
    type SerializeTuple = SeqBuilder;
    type SerializeTupleStruct = SeqBuilder;
    type SerializeTupleVariant = SeqBuilder;
    type SerializeMap = MapBuilder;
    type SerializeStruct = MapBuilder;
    type SerializeStructVariant = MapBuilder;

    fn serialize_bool(self, v: bool) -> Result<Value, Self::Error> {
        Ok(Value::Bool(v))
    }
    fn serialize_i8(self, v: i8) -> Result<Value, Self::Error> {
        Ok(Value::Number(f64::from(v)))
    }
    fn serialize_i16(self, v: i16) -> Result<Value, Self::Error> {
        Ok(Value::Number(f64::from(v)))
    }
    fn serialize_i32(self, v: i32) -> Result<Value, Self::Error> {
        Ok(Value::Number(f64::from(v)))
    }
    fn serialize_i64(self, v: i64) -> Result<Value, Self::Error> {
        Ok(Value::Number(v as f64))
    }
    fn serialize_u8(self, v: u8) -> Result<Value, Self::Error> {
        Ok(Value::Number(f64::from(v)))
    }
    fn serialize_u16(self, v: u16) -> Result<Value, Self::Error> {
        Ok(Value::Number(f64::from(v)))
    }
    fn serialize_u32(self, v: u32) -> Result<Value, Self::Error> {
        Ok(Value::Number(f64::from(v)))
    }
    fn serialize_u64(self, v: u64) -> Result<Value, Self::Error> {
        Ok(Value::Number(v as f64))
    }
    fn serialize_f32(self, v: f32) -> Result<Value, Self::Error> {
        self.serialize_f64(f64::from(v))
    }
    fn serialize_f64(self, v: f64) -> Result<Value, Self::Error> {
        Ok(if v.is_finite() {
            Value::Number(v)
        } else {
            Value::Null
        })
    }
    fn serialize_char(self, v: char) -> Result<Value, Self::Error> {
        Ok(text(v.encode_utf8(&mut [0; 4])))
    }
    fn serialize_str(self, v: &str) -> Result<Value, Self::Error> {
        Ok(text(v))
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<Value, Self::Error> {
        Ok(Value::Array(Shared::new(
            v.iter().map(|b| Value::Number(f64::from(*b))).collect(),
        )))
    }
    fn serialize_none(self) -> Result<Value, Self::Error> {
        Ok(Value::Null)
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Value, Self::Error> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<Value, Self::Error> {
        Ok(Value::Null)
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<Value, Self::Error> {
        Ok(Value::Null)
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<Value, Self::Error> {
        Ok(text(variant))
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<Value, Self::Error> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Value, Self::Error> {
        let mut fields = ValueMap::default();
        fields.insert(variant.to_owned().into(), value.serialize(self)?);
        Ok(Value::Object(Shared::new(fields)))
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<SeqBuilder, Self::Error> {
        Ok(SeqBuilder {
            items: Vec::with_capacity(len.unwrap_or(0)),
            canonical: self.canonical,
            variant: None,
        })
    }
    fn serialize_tuple(self, len: usize) -> Result<SeqBuilder, Self::Error> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        len: usize,
    ) -> Result<SeqBuilder, Self::Error> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<SeqBuilder, Self::Error> {
        Ok(SeqBuilder {
            items: Vec::with_capacity(len),
            canonical: self.canonical,
            variant: Some(variant),
        })
    }
    fn serialize_map(self, len: Option<usize>) -> Result<MapBuilder, Self::Error> {
        Ok(MapBuilder {
            fields: ValueMap::with_capacity_and_hasher(len.unwrap_or(0), Default::default()),
            pending: None,
            canonical: self.canonical,
            variant: None,
        })
    }
    fn serialize_struct(self, _: &'static str, len: usize) -> Result<MapBuilder, Self::Error> {
        self.serialize_map(Some(len))
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<MapBuilder, Self::Error> {
        Ok(MapBuilder {
            fields: ValueMap::with_capacity_and_hasher(len, Default::default()),
            pending: None,
            canonical: self.canonical,
            variant: Some(variant),
        })
    }
}

/// An object key: text, or a number, boolean or character written as text, as `serde_json` writes them.
struct KeySerializer;

fn key_error() -> ValueSerializeError {
    ser::Error::custom("an object key must be text")
}

macro_rules! key_as_text {
    ($($method:ident($ty:ty)),* $(,)?) => {
        $(fn $method(self, v: $ty) -> Result<String, ValueSerializeError> { Ok(v.to_string()) })*
    };
}

macro_rules! key_refused {
    ($($method:ident($($arg:ty),*) -> $ret:ty),* $(,)?) => {
        $(fn $method(self, $(_: $arg),*) -> Result<$ret, ValueSerializeError> { Err(key_error()) })*
    };
}

impl ser::Serializer for KeySerializer {
    type Ok = String;
    type Error = ValueSerializeError;
    type SerializeSeq = ser::Impossible<String, ValueSerializeError>;
    type SerializeTuple = ser::Impossible<String, ValueSerializeError>;
    type SerializeTupleStruct = ser::Impossible<String, ValueSerializeError>;
    type SerializeTupleVariant = ser::Impossible<String, ValueSerializeError>;
    type SerializeMap = ser::Impossible<String, ValueSerializeError>;
    type SerializeStruct = ser::Impossible<String, ValueSerializeError>;
    type SerializeStructVariant = ser::Impossible<String, ValueSerializeError>;

    key_as_text!(
        serialize_bool(bool),
        serialize_i8(i8),
        serialize_i16(i16),
        serialize_i32(i32),
        serialize_i64(i64),
        serialize_u8(u8),
        serialize_u16(u16),
        serialize_u32(u32),
        serialize_u64(u64),
        serialize_char(char),
        serialize_str(&str),
    );
    key_refused!(
        serialize_f32(f32) -> String,
        serialize_f64(f64) -> String,
        serialize_bytes(&[u8]) -> String,
        serialize_none() -> String,
        serialize_unit() -> String,
        serialize_unit_struct(&'static str) -> String,
        serialize_seq(Option<usize>) -> Self::SerializeSeq,
        serialize_tuple(usize) -> Self::SerializeTuple,
        serialize_tuple_struct(&'static str, usize) -> Self::SerializeTupleStruct,
        serialize_tuple_variant(&'static str, u32, &'static str, usize) -> Self::SerializeTupleVariant,
        serialize_map(Option<usize>) -> Self::SerializeMap,
        serialize_struct(&'static str, usize) -> Self::SerializeStruct,
        serialize_struct_variant(&'static str, u32, &'static str, usize) -> Self::SerializeStructVariant,
    );

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<String, Self::Error> {
        value.serialize(self)
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<String, Self::Error> {
        Ok(variant.to_owned())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<String, Self::Error> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<String, Self::Error> {
        Err(key_error())
    }
}
