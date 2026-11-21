//! A value's structural key (`value/key`): one deterministic string, equal for equal values and different for unequal
//! ones, in one walk (O(size)). A record's field order does not change it; a list and a record never share a key. A NaN
//! has one key though it is unequal to itself; a lambda has none.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::value::{Value, ValueMap};

/// The structural key of `value`, or the reason it has none (a lambda inside it).
pub fn structural_key(value: &Value) -> Result<String, String> {
    let mut out = String::new();
    write_key(value, &mut out)?;
    Ok(out)
}

fn write_key(value: &Value, out: &mut String) -> Result<(), String> {
    use core::fmt::Write;
    match value {
        Value::Null => out.push('n'),
        Value::Bool(true) => out.push('t'),
        Value::Bool(false) => out.push('f'),
        Value::Int(i) => {
            let _ = write!(out, "i{i};");
        }
        Value::Number(f) => {
            let bound = -(i64::MIN as f64);
            if f.fract() == 0.0 && *f >= -bound && *f < bound {
                let _ = write!(out, "i{};", *f as i64);
            } else {
                let _ = write!(out, "d{f:?};");
            }
        }
        Value::Bytes(bytes) => {
            let _ = write!(out, "b{}:", bytes.len());
            for byte in bytes.iter() {
                let _ = write!(out, "{byte:02x}");
            }
        }
        Value::String(text) => {
            let _ = write!(out, "s{}:{}", text.len(), text.as_str());
        }
        Value::Array(items) => {
            out.push('[');
            for item in items.iter() {
                write_key(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => write_record_key(map, "{", out)?,
        Value::Sealed(sealed) => {
            let open = format!("S{}:{}{{", sealed.kind().len(), sealed.kind());
            write_record_key(sealed.fields(), &open, out)?
        }
        Value::Lambda(_) => return Err("value/key: a lambda has no structural key".to_string()),
        Value::Blob(blob) => {
            let _ = write!(out, "B{};", blob.id());
        }
    }
    Ok(())
}

fn write_record_key(map: &ValueMap, open: &str, out: &mut String) -> Result<(), String> {
    use core::fmt::Write;
    let mut fields: Vec<(&str, &Value)> = map.iter().map(|(key, item)| (key.as_str(), item)).collect();
    fields.sort_unstable_by(|a, b| a.0.cmp(b.0));
    out.push_str(open);
    for (key, item) in fields {
        let _ = write!(out, "{}:{}", key.len(), key);
        write_key(item, out)?;
    }
    out.push('}');
    Ok(())
}
