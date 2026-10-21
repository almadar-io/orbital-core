//! `bit/*` operators over 32-bit unsigned words. Out-of-range operands and counts are refused, never wrapped.

use super::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(form: &str) -> EvalResult {
        let expr: SExpression = serde_json::from_str(form).expect("s-expression");
        eval_expr(&expr, &BindingContext::default())
    }

    #[test]
    fn values() {
        let cases: &[(&str, f64)] = &[
            (r#"["bit/xor", 0, 0]"#, 0.0),
            (r#"["bit/xor", 4294967295, 0]"#, 4294967295.0),
            (r#"["bit/xor", 4294967295, 4294967295]"#, 0.0),
            (r#"["bit/xor", 305419896, 2596069104]"#, 2290649224.0),
            (r#"["bit/xor", 1, 2, 4, 8]"#, 15.0),
            (r#"["bit/xor", 2147483648, 2147483648, 2147483648]"#, 2147483648.0),
            (r#"["bit/and", 4294967295, 2147483648]"#, 2147483648.0),
            (r#"["bit/and", 305419896, 255]"#, 120.0),
            (r#"["bit/and", 4042322160, 267390960, 4294901760]"#, 15728640.0),
            (r#"["bit/or", 2147483648, 1]"#, 2147483649.0),
            (r#"["bit/or", 0, 0]"#, 0.0),
            (r#"["bit/or", 252645135, 4042322160]"#, 4294967295.0),
            (r#"["bit/not", 0]"#, 4294967295.0),
            (r#"["bit/not", 4294967295]"#, 0.0),
            (r#"["bit/not", 2147483648]"#, 2147483647.0),
            (r#"["bit/not", 305419896]"#, 3989547399.0),
            (r#"["bit/shl", 1, 0]"#, 1.0),
            (r#"["bit/shl", 1, 31]"#, 2147483648.0),
            (r#"["bit/shl", 4294967295, 1]"#, 4294967294.0),
            (r#"["bit/shl", 2147483648, 1]"#, 0.0),
            (r#"["bit/shl", 305419896, 4]"#, 591751040.0),
            (r#"["bit/shr", 2147483648, 31]"#, 1.0),
            (r#"["bit/shr", 4294967295, 0]"#, 4294967295.0),
            (r#"["bit/shr", 4294967295, 31]"#, 1.0),
            (r#"["bit/shr", 305419896, 8]"#, 1193046.0),
            (r#"["bit/shr", 1, 1]"#, 0.0),
            (r#"["bit/rotl", 2147483648, 1]"#, 1.0),
            (r#"["bit/rotl", 1, 31]"#, 2147483648.0),
            (r#"["bit/rotl", 305419896, 0]"#, 305419896.0),
            (r#"["bit/rotl", 305419896, 8]"#, 878082066.0),
            (r#"["bit/rotl", 4294967295, 13]"#, 4294967295.0),
            (r#"["bit/rotr", 1, 1]"#, 2147483648.0),
            (r#"["bit/rotr", 2147483648, 31]"#, 1.0),
            (r#"["bit/rotr", 305419896, 0]"#, 305419896.0),
            (r#"["bit/rotr", 305419896, 8]"#, 2014458966.0),
            (r#"["bit/rotr", 1779033703, 7]"#, 3470005196.0),
            (r#"["bit/rotr", 1779033703, 18]"#, 2040126082.0),
        ];
        for (form, expected) in cases {
            match eval(form) {
                Ok(Value::Number(n)) => assert_eq!(n, *expected, "{form}"),
                other => panic!("{form}: expected {expected}, got {other:?}"),
            }
        }
    }

    #[test]
    fn nested_results_feed_back() {
        let h0: u32 = 1779033703;
        let sigma0 = h0.rotate_right(2) ^ h0.rotate_right(13) ^ h0.rotate_right(22);
        match eval(
            r#"["bit/xor", ["bit/rotr", 1779033703, 2], ["bit/rotr", 1779033703, 13], ["bit/rotr", 1779033703, 22]]"#,
        ) {
            Ok(Value::Number(n)) => assert_eq!(n, sigma0 as f64),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn refusals() {
        let cases: &[(&str, &str)] = &[
            (r#"["bit/xor", 4294967296, 0]"#, "bit/xor: invalid operand 4294967296: expected a whole number 0..4294967295"),
            (r#"["bit/and", -1, 0]"#, "bit/and: invalid operand -1: expected a whole number 0..4294967295"),
            (r#"["bit/or", 1.5, 0]"#, "bit/or: invalid operand 1.5: expected a whole number 0..4294967295"),
            (r#"["bit/not", null]"#, "bit/not: invalid operand null: expected a whole number 0..4294967295"),
            (r#"["bit/xor", "1", 0]"#, "bit/xor: invalid operand"),
            (r#"["bit/xor", 0, 1, 4294967296]"#, "bit/xor: invalid operand 4294967296"),
            (r#"["bit/shl", 1, 32]"#, "bit/shl: invalid count 32: expected a whole number 0..31"),
            (r#"["bit/shr", 1, -1]"#, "bit/shr: invalid count -1: expected a whole number 0..31"),
            (r#"["bit/rotl", 1, 0.5]"#, "bit/rotl: invalid count 0.5"),
            (r#"["bit/rotl", 1, 32]"#, "bit/rotl: invalid count 32"),
            (r#"["bit/rotr", 1, 32]"#, "bit/rotr: invalid count 32"),
            (r#"["bit/rotr", 1, -1]"#, "bit/rotr: invalid count -1"),
            (r#"["bit/rotr", 4294967296, 1]"#, "bit/rotr: invalid operand 4294967296"),
            (r#"["bit/shr", 4294967296, 0]"#, "bit/shr: invalid operand 4294967296"),
        ];
        for (form, message) in cases {
            match eval(form) {
                Err(e) => assert!(e.to_string().contains(message), "{form}: {e}"),
                Ok(v) => panic!("{form}: expected an error, got {v:?}"),
            }
        }
    }

    #[test]
    fn arity() {
        for form in [
            r#"["bit/xor", 1]"#,
            r#"["bit/and", 1]"#,
            r#"["bit/or"]"#,
            r#"["bit/not", 1, 2]"#,
            r#"["bit/shl", 1]"#,
            r#"["bit/rotr", 1, 2, 3]"#,
        ] {
            match eval(form) {
                Err(OrbitalError::Eval(EvalError::ArityMismatch { .. })) => {}
                other => panic!("{form}: expected ArityMismatch, got {other:?}"),
            }
        }
    }
}
