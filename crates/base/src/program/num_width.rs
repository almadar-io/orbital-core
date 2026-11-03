//! Fixed-width number types: `i8`…`u64`, `f32`, `f64`. A width is a checked range over the run-time `int` and
//! `number` values, never a value of its own; `i64` is `int` and `f64` is `number`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::value::Value;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NumWidth {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
}

impl NumWidth {
    pub const ALL: &'static [NumWidth] = &[
        NumWidth::I8,
        NumWidth::I16,
        NumWidth::I32,
        NumWidth::I64,
        NumWidth::U8,
        NumWidth::U16,
        NumWidth::U32,
        NumWidth::U64,
        NumWidth::F32,
        NumWidth::F64,
    ];

    /// The `.lolo` spelling.
    pub fn name(self) -> &'static str {
        match self {
            NumWidth::I8 => "i8",
            NumWidth::I16 => "i16",
            NumWidth::I32 => "i32",
            NumWidth::I64 => "i64",
            NumWidth::U8 => "u8",
            NumWidth::U16 => "u16",
            NumWidth::U32 => "u32",
            NumWidth::U64 => "u64",
            NumWidth::F32 => "f32",
            NumWidth::F64 => "f64",
        }
    }

    pub fn from_name(name: &str) -> Option<NumWidth> {
        NumWidth::ALL.iter().copied().find(|w| w.name() == name)
    }

    /// The widths that are their own type; `i64` is `int` and `f64` is `number`.
    pub fn is_sized(self) -> bool {
        !matches!(self, NumWidth::I64 | NumWidth::F64)
    }

    pub fn is_float(self) -> bool {
        matches!(self, NumWidth::F32 | NumWidth::F64)
    }

    /// The field type this width declares.
    pub fn field_type(self) -> crate::schema::types::FieldType {
        use crate::schema::types::FieldType;
        match self {
            NumWidth::I64 => FieldType::Int,
            NumWidth::F64 => FieldType::Number,
            sized => FieldType::Sized(sized),
        }
    }

    /// The IR type this width declares.
    pub fn oir_type(self) -> crate::ir::types::OirType {
        use crate::ir::types::OirType;
        match self {
            NumWidth::I64 => OirType::Integer,
            NumWidth::F64 => OirType::Number,
            width => OirType::Sized { width },
        }
    }

    /// The inclusive integer range; `u64` stops at `i64::MAX`, the largest run-time `int`.
    pub fn int_range(self) -> Option<(i64, i64)> {
        Some(match self {
            NumWidth::I8 => (i8::MIN.into(), i8::MAX.into()),
            NumWidth::I16 => (i16::MIN.into(), i16::MAX.into()),
            NumWidth::I32 => (i32::MIN.into(), i32::MAX.into()),
            NumWidth::I64 => (i64::MIN, i64::MAX),
            NumWidth::U8 => (0, u8::MAX.into()),
            NumWidth::U16 => (0, u16::MAX.into()),
            NumWidth::U32 => (0, u32::MAX.into()),
            NumWidth::U64 => (0, i64::MAX),
            NumWidth::F32 | NumWidth::F64 => return None,
        })
    }

    /// Whether `value` lies in this width: an exact int in range, or for a float width a finite number in range.
    pub fn fits(self, value: &Value) -> bool {
        match self.int_range() {
            Some((low, high)) => value.as_int().is_some_and(|n| (low..=high).contains(&n)),
            None => value.as_number().is_some_and(|n| match self {
                NumWidth::F32 => n.is_finite() && n.abs() <= f32::MAX as f64,
                _ => n.is_finite(),
            }),
        }
    }

    /// Whether every value of `self` is a value of `to`.
    pub fn widens_to(self, to: NumWidth) -> bool {
        match (self.int_range(), to.int_range()) {
            (Some((low, high)), Some((to_low, to_high))) => to_low <= low && high <= to_high,
            (None, None) => self == NumWidth::F32 || to == NumWidth::F64,
            _ => false,
        }
    }
}

impl core::fmt::Display for NumWidth {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}

impl Serialize for NumWidth {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for NumWidth {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        NumWidth::from_name(&raw)
            .filter(|w| w.is_sized())
            .ok_or_else(|| serde::de::Error::custom(format!("unknown width `{raw}` (i64 is int, f64 is number)")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_width_has_one_spelling_and_its_edges_fit() {
        for &w in NumWidth::ALL {
            assert_eq!(NumWidth::from_name(w.name()), Some(w));
            if let Some((low, high)) = w.int_range() {
                assert!(w.fits(&Value::Int(low)) && w.fits(&Value::Int(high)), "{w}");
                if low > i64::MIN {
                    assert!(!w.fits(&Value::Int(low - 1)), "{w} below");
                }
                if high < i64::MAX {
                    assert!(!w.fits(&Value::Int(high + 1)), "{w} above");
                }
            }
        }
        assert_eq!(NumWidth::from_name("int"), None);
    }

    #[test]
    fn an_integer_width_takes_whole_numbers_and_refuses_fractions_and_non_numbers() {
        assert!(NumWidth::U8.fits(&Value::Number(255.0)));
        assert!(!NumWidth::U8.fits(&Value::Number(1.5)));
        assert!(!NumWidth::U8.fits(&Value::Int(-1)));
        assert!(!NumWidth::I32.fits(&Value::String("1".to_string().into())));
    }

    #[test]
    fn f32_takes_finite_numbers_in_its_range_only() {
        assert!(NumWidth::F32.fits(&Value::Number(0.1)));
        assert!(NumWidth::F32.fits(&Value::Int(7)));
        assert!(!NumWidth::F32.fits(&Value::Number(1e39)));
        assert!(!NumWidth::F32.fits(&Value::Number(f64::NAN)));
        assert!(NumWidth::F64.fits(&Value::Number(1e39)));
    }

    #[test]
    fn widening_follows_the_ranges_and_never_narrows() {
        assert!(NumWidth::U8.widens_to(NumWidth::I16));
        assert!(NumWidth::U32.widens_to(NumWidth::I64));
        assert!(NumWidth::U64.widens_to(NumWidth::I64));
        assert!(NumWidth::F32.widens_to(NumWidth::F64));
        assert!(!NumWidth::I8.widens_to(NumWidth::U16), "control: a negative does not fit unsigned");
        assert!(!NumWidth::U16.widens_to(NumWidth::I16));
        assert!(!NumWidth::I32.widens_to(NumWidth::F32), "control: an int width is not a float width");
        assert!(!NumWidth::F64.widens_to(NumWidth::F32));
    }

    #[test]
    fn the_wire_names_only_the_sized_widths() {
        assert_eq!(serde_json::to_string(&NumWidth::U8).unwrap(), "\"u8\"");
        assert_eq!(serde_json::from_str::<NumWidth>("\"i16\"").unwrap(), NumWidth::I16);
        assert!(serde_json::from_str::<NumWidth>("\"i64\"").is_err(), "control: i64 is spelled int");
        assert!(serde_json::from_str::<NumWidth>("\"f64\"").is_err());
    }
}
