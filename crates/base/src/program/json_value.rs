use crate::str_utils::js_number_string;
use crate::value::Value;

/// JS-semantics conversion for stringify: integer-valued numbers serialize
/// without a decimal point (`JSON.stringify(1)` → `"1"`, never `"1.0"`),
/// matching `to_string_value`'s integer rule. A lambda has no JSON form (JS
/// `JSON.stringify(fn)` → undefined) and serializes as null.
pub fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Number(n) => {
            if n.fract() == 0.0 && n.abs() < i64::MAX as f64 {
                serde_json::Value::Number((*n as i64).into())
            } else {
                serde_json::Number::from_f64(*n)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            }
        }
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Array(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect(),
        ),
        Value::Lambda { .. } => serde_json::Value::Null,
    }
}
/// JSON text as JavaScript's `JSON.stringify` writes it: every number through [`js_number_string`], a non-finite
/// number as `null`.
pub fn js_json_string<T: serde::Serialize + ?Sized>(value: &T) -> String {
    let mut out = Vec::new();
    value
        .serialize(&mut serde_json::Serializer::with_formatter(
            &mut out,
            JsNumberFormatter,
        ))
        .expect("JSON text of an in-memory value always writes");
    String::from_utf8(out).expect("serde_json writes UTF-8")
}

/// [`js_json_string`] with two-space indentation, as `JSON.stringify(v, null, 2)` writes it.
pub fn js_json_string_pretty<T: serde::Serialize + ?Sized>(value: &T) -> String {
    let mut out = Vec::new();
    value
        .serialize(&mut serde_json::Serializer::with_formatter(
            &mut out,
            JsPrettyFormatter(serde_json::ser::PrettyFormatter::new()),
        ))
        .expect("JSON text of an in-memory value always writes");
    String::from_utf8(out).expect("serde_json writes UTF-8")
}

struct JsPrettyFormatter(serde_json::ser::PrettyFormatter<'static>);

impl serde_json::ser::Formatter for JsPrettyFormatter {
    fn write_f64<W: ?Sized + std::io::Write>(&mut self, writer: &mut W, value: f64) -> std::io::Result<()> {
        writer.write_all(js_number_string(value).as_bytes())
    }
    fn write_f32<W: ?Sized + std::io::Write>(&mut self, writer: &mut W, value: f32) -> std::io::Result<()> {
        self.write_f64(writer, f64::from(value))
    }
    fn begin_array<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.0.begin_array(writer)
    }
    fn end_array<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.0.end_array(writer)
    }
    fn begin_array_value<W: ?Sized + std::io::Write>(&mut self, writer: &mut W, first: bool) -> std::io::Result<()> {
        self.0.begin_array_value(writer, first)
    }
    fn end_array_value<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.0.end_array_value(writer)
    }
    fn begin_object<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.0.begin_object(writer)
    }
    fn end_object<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.0.end_object(writer)
    }
    fn begin_object_key<W: ?Sized + std::io::Write>(&mut self, writer: &mut W, first: bool) -> std::io::Result<()> {
        self.0.begin_object_key(writer, first)
    }
    fn begin_object_value<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.0.begin_object_value(writer)
    }
    fn end_object_value<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        self.0.end_object_value(writer)
    }
}

struct JsNumberFormatter;

impl serde_json::ser::Formatter for JsNumberFormatter {
    fn write_f64<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        value: f64,
    ) -> std::io::Result<()> {
        writer.write_all(js_number_string(value).as_bytes())
    }

    fn write_f32<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        value: f32,
    ) -> std::io::Result<()> {
        self.write_f64(writer, f64::from(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_json_string_writes_numbers_as_ecmascript() {
        let value = Value::Array(vec![
            Value::Number(1e19),
            Value::Number(1e-6),
            Value::Number(1.0),
            Value::Number(-0.0),
            Value::Number(1e21),
            Value::Number(f64::NAN),
        ]);
        assert_eq!(
            js_json_string(&value),
            "[10000000000000000000,0.000001,1,0,1e+21,null]"
        );
        assert_eq!(
            js_json_string(&value_to_json(&value)),
            "[10000000000000000000,0.000001,1,0,1e+21,null]"
        );
    }

    #[test]
    fn control_serde_json_default_number_text_is_not_ecmascript() {
        assert_eq!(serde_json::to_string(&Value::Number(1e19)).unwrap(), "1e+19");
    }
}
