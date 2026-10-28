//! CSV operators.
//!
//! `(csv/parse text)` — pure, deterministic: CSV text → one `Map string string`
//! per data row, keyed by the header row. RFC 4180 quoting (`""` escapes a
//! quote; a quoted field may hold commas and line breaks); records end in CRLF,
//! LF or CR; a blank line is skipped, a short row's missing cells are `""`, a
//! long row's extra cells are dropped, a leading BOM is ignored. Empty text and
//! header-only text → `[]`; non-string input is a type mismatch. The
//! `@almadar/evaluator` twin is `std/csv.ts`; both read `operator-types.json`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::macros::*;
use super::*;

fn records(text: &str) -> Vec<Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();

    let mut end_record = |record: &mut Vec<String>, field: &mut String, quoted: &mut bool| {
        if record.is_empty() && field.is_empty() && !*quoted {
            return;
        }
        record.push(std::mem::take(field));
        out.push(std::mem::take(record));
        *quoted = false;
    };

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' && field.is_empty() {
            in_quotes = true;
            quoted = true;
        } else if c == ',' {
            record.push(std::mem::take(&mut field));
            quoted = false;
        } else if c == '\r' || c == '\n' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            end_record(&mut record, &mut field, &mut quoted);
        } else {
            field.push(c);
        }
    }
    end_record(&mut record, &mut field, &mut quoted);
    out
}

pub fn parse_csv(text: &str) -> Vec<Value> {
    let mut rows = records(text).into_iter();
    let Some(header) = rows.next() else {
        return Vec::new();
    };
    rows.map(|cells| {
        let mut row = ValueMap::default();
        for (index, name) in header.iter().enumerate() {
            row.insert(
                name.clone(),
                Value::string(cells.get(index).cloned().unwrap_or_default()),
            );
        }
        Value::object(row)
    })
    .collect()
}

pub struct CsvParseOp;
impl Operator for CsvParseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "csv/parse", 1);
        let input = eval_expr(&args[0], ctx)?;
        Ok(Value::array(parse_csv(input.text()?)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &str) -> Vec<Vec<(String, String)>> {
        parse_csv(text)
            .into_iter()
            .map(|row| match row {
                Value::Object(map) => map
                    .into_iter()
                    .map(|(k, v)| match v {
                        Value::String(s) => (k, s.into_inner()),
                        other => panic!("cell is not a string: {other:?}"),
                    })
                    .collect(),
                other => panic!("row is not an object: {other:?}"),
            })
            .collect()
    }

    fn pairs(cells: &[(&str, &str)]) -> Vec<(String, String)> {
        cells
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn plain_rows_are_keyed_by_the_header() {
        assert_eq!(
            rows("name,age\nAda,36\nGrace,45"),
            vec![
                pairs(&[("name", "Ada"), ("age", "36")]),
                pairs(&[("name", "Grace"), ("age", "45")])
            ]
        );
    }

    #[test]
    fn quoted_commas_and_escaped_quotes_and_line_breaks() {
        assert_eq!(
            rows("a,b\n\"x, y\",\"she said \"\"hi\"\"\"\n\"line1\nline2\",z"),
            vec![
                pairs(&[("a", "x, y"), ("b", "she said \"hi\"")]),
                pairs(&[("a", "line1\nline2"), ("b", "z")])
            ]
        );
    }

    #[test]
    fn crlf_cr_and_trailing_newline() {
        let expected = vec![pairs(&[("a", "1"), ("b", "2")])];
        assert_eq!(rows("a,b\r\n1,2\r\n"), expected);
        assert_eq!(rows("a,b\r1,2\r"), expected);
        assert_eq!(rows("a,b\n1,2\n"), expected);
        assert_eq!(rows("a,b\n1,2"), expected);
    }

    #[test]
    fn empty_and_header_only_are_no_rows() {
        assert!(rows("").is_empty());
        assert!(rows("a,b").is_empty());
        assert!(rows("a,b\n").is_empty());
        assert!(rows("\n\n").is_empty());
    }

    #[test]
    fn blank_lines_are_skipped_and_short_long_rows_are_normalized() {
        assert_eq!(
            rows("a,b\n\n1\n2,3,4\n"),
            vec![
                pairs(&[("a", "1"), ("b", "")]),
                pairs(&[("a", "2"), ("b", "3")])
            ]
        );
    }

    #[test]
    fn empty_cells_and_quoted_empty_field_are_kept() {
        assert_eq!(
            rows("a,b,c\n,\"\",x"),
            vec![pairs(&[("a", ""), ("b", ""), ("c", "x")])]
        );
        assert_eq!(rows("a\n\"\""), vec![pairs(&[("a", "")])]);
    }

    #[test]
    fn bom_is_ignored() {
        assert_eq!(rows("\u{feff}a\n1"), vec![pairs(&[("a", "1")])]);
    }

    #[test]
    fn non_string_input_is_a_type_mismatch_and_text_parses() {
        let run = |input: serde_json::Value| {
            let expr: SExpression =
                serde_json::from_value(serde_json::json!(["csv/parse", input])).unwrap();
            eval_expr(&expr, &BindingContext::default())
        };
        assert!(run(serde_json::Value::Null).is_err());
        assert!(run(serde_json::json!(5)).is_err());
        assert_eq!(
            run(serde_json::json!("a\n1"))
                .unwrap()
                .as_array()
                .map(Vec::len),
            Some(1)
        );
    }
}
