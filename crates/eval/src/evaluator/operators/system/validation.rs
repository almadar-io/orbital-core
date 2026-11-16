//! Validation operators and the value constraints they check.

use super::*;

// ============================================================================
// Phase 2.6: Validation Operations
// ============================================================================

/// validate/required - Check not null/empty
pub struct ValidateRequiredOp;
impl StrictOperator for ValidateRequiredOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/required", 1);
        let val = args[0];
        let is_valid = match val {
            Value::Null => false,
            Value::String(s) => !s.is_empty(),
            Value::Bytes(b) => !b.is_empty(),
            Value::Array(a) => !a.is_empty(),
            _ => true,
        };
        Ok(Value::Bool(is_valid))
    }
}

/// validate/string - Type check
pub struct ValidateStringOp;
impl StrictOperator for ValidateStringOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/string", 1);
        let val = args[0];
        Ok(Value::Bool(TypePredicate::String.matches_value(val)))
    }
}

/// validate/number - Type check
pub struct ValidateNumberOp;
impl StrictOperator for ValidateNumberOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/number", 1);
        let val = args[0];
        Ok(Value::Bool(TypePredicate::Number.matches_value(val)))
    }
}

/// validate/boolean - Type check
pub struct ValidateBooleanOp;
impl StrictOperator for ValidateBooleanOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/boolean", 1);
        let val = args[0];
        Ok(Value::Bool(TypePredicate::Boolean.matches_value(val)))
    }
}

/// validate/array - Type check
pub struct ValidateArrayOp;
impl StrictOperator for ValidateArrayOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/array", 1);
        let val = args[0];
        Ok(Value::Bool(TypePredicate::Array.matches_value(val)))
    }
}

/// validate/object - Type check
pub struct ValidateObjectOp;
impl StrictOperator for ValidateObjectOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/object", 1);
        let val = args[0];
        Ok(Value::Bool(TypePredicate::Object.matches_value(val)))
    }
}

/// Whether `value` matches the pattern the installed operator table declares for the semantic validator `operator`
/// (`validate/email`, `validate/url`, `validate/phone`, `validate/uuid`). The pattern is `@almadar/core`
/// `SEMANTIC_STRING_PATTERNS`, carried by the table; a table that lacks it is an error, never a fallback.
pub fn matches_semantic_pattern(operator: &str, value: &str) -> Result<bool, OrbitalError> {
    static COMPILED: orbital_core_base::sync::OnceLock<
        orbital_core_base::sync::Mutex<std::collections::HashMap<String, regex::Regex>>,
    > = orbital_core_base::sync::OnceLock::new();
    let cache = COMPILED.get_or_init(Default::default);
    let mut cache = cache.lock();
    if !cache.contains_key(operator) {
        let pattern = orbital_core_base::language::registry::lookup_operator(operator)
            .map_err(|e| OrbitalError::Eval(EvalError::InvalidArgument(format!("{}: {e}", e.code()))))?
            .and_then(|meta| meta.pattern.as_deref())
            .ok_or_else(|| {
                OrbitalError::Eval(EvalError::InvalidArgument(format!(
                    "{operator}: the installed operator table declares no `pattern`"
                )))
            })?;
        let regex = super::string::compile_shared_regex(pattern).ok_or_else(|| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "{operator}: the declared pattern is outside the shared regex dialect"
            )))
        })?;
        cache.insert(operator.to_string(), regex);
    }
    Ok(cache.get(operator).is_some_and(|re| re.is_match(value)))
}

/// Does `value` satisfy the semantic domain `kind` (`email`, `url`, `image`, `phone`, `uuid`; `image` is a URL)?
/// The same definition the `validate/*` operators evaluate; any other kind has no value rule.
pub fn semantic_string_value_valid(kind: &str, value: &str) -> Result<bool, OrbitalError> {
    match kind {
        "email" => matches_semantic_pattern("validate/email", value),
        "url" | "image" => matches_semantic_pattern("validate/url", value),
        "phone" => matches_semantic_pattern("validate/phone", value),
        "uuid" => matches_semantic_pattern("validate/uuid", value),
        _ => Ok(true),
    }
}

/// validate/email - the `pattern` the operator table declares for it
pub struct ValidateEmailOp;
impl StrictOperator for ValidateEmailOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/email", 1);
        let val = args[0];
        let val = val.text()?;
        Ok(Value::Bool(matches_semantic_pattern(
            "validate/email",
            val,
        )?))
    }
}

/// validate/url - the `pattern` the operator table declares for it
pub struct ValidateUrlOp;
impl StrictOperator for ValidateUrlOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/url", 1);
        let val = args[0];
        let val = val.text()?;
        Ok(Value::Bool(matches_semantic_pattern("validate/url", val)?))
    }
}

/// The length of a `string | [T]` value: characters of a string, elements of a list; any other kind is an error.
fn length_of(val: &Value) -> Result<usize, OrbitalError> {
    match val {
        Value::String(s) => Ok(s.chars().count()),
        Value::Array(a) => Ok(a.len()),
        other => Err(other.kind_error("string or array").into()),
    }
}

/// validate/minLength - Minimum string length
pub struct ValidateMinLengthOp;
impl StrictOperator for ValidateMinLengthOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/minLength", >= 2);
        let len = length_of(args[0])?;
        let min = require_number!(args[1]);

        Ok(Value::Bool(len as f64 >= min))
    }
}

/// validate/maxLength - Maximum string length
pub struct ValidateMaxLengthOp;
impl StrictOperator for ValidateMaxLengthOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/maxLength", >= 2);
        let len = length_of(args[0])?;
        let max = require_number!(args[1]);

        Ok(Value::Bool(len as f64 <= max))
    }
}

/// validate/pattern - Match a regex pattern of the shared dialect (see `translate_shared_regex`).
/// An invalid or unsupported pattern is false; a non-string value or pattern is a type mismatch.
pub struct ValidatePatternOp;
impl StrictOperator for ValidatePatternOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/pattern", >= 2);
        let val = args[0];
        let pattern = args[1];
        let (val, pattern) = (val.text()?, pattern.text()?);
        Ok(Value::Bool(
            super::string::compile_shared_regex(pattern).is_some_and(|re| re.is_match(val)),
        ))
    }
}

// ============================================================================
// Phase 3.6: Validation Advanced
// ============================================================================

/// validate/uuid - the `pattern` the operator table declares for it
pub struct ValidateUuidOp;
impl StrictOperator for ValidateUuidOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/uuid", 1);
        let val = args[0];
        let val = val.text()?;
        Ok(Value::Bool(matches_semantic_pattern("validate/uuid", val)?))
    }
}

/// validate/date - Valid date string
pub struct ValidateDateOp;
impl StrictOperator for ValidateDateOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/date", 1);
        let val = args[0];
        let Value::String(val) = val else {
            return Ok(Value::Bool(false));
        };

        // Try common date formats: YYYY-MM-DD or YYYY/MM/DD
        let parts: Vec<&str> = if val.contains('-') {
            val.split('-').collect()
        } else if val.contains('/') {
            val.split('/').collect()
        } else {
            return Ok(Value::Bool(false));
        };

        if parts.len() != 3 {
            return Ok(Value::Bool(false));
        }

        let year: Option<i32> = parts[0].parse().ok();
        let month: Option<u32> = parts[1].parse().ok();
        let day: Option<u32> = parts[2].parse().ok();

        let is_valid = year.is_some()
            && month.map(|m| (1..=12).contains(&m)).unwrap_or(false)
            && day.map(|d| (1..=31).contains(&d)).unwrap_or(false);

        Ok(Value::Bool(is_valid))
    }
}

/// validate/length - Exact length (works with strings and arrays)
pub struct ValidateLengthOp;
impl StrictOperator for ValidateLengthOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/length", >= 2);
        let actual_len = length_of(args[0])?;
        let expected_len = require_number!(args[1]);

        Ok(Value::Bool(actual_len as f64 == expected_len))
    }
}

/// validate/range - Value in numeric range
pub struct ValidateRangeOp;
impl StrictOperator for ValidateRangeOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/range", >= 3);
        let val = require_number!(args[0]);
        let min = require_number!(args[1]);
        let max = require_number!(args[2]);

        Ok(Value::Bool(val >= min && val <= max))
    }
}

/// validate/oneOf - Value in set
pub struct ValidateOneOfOp;
impl StrictOperator for ValidateOneOfOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/oneOf", >= 2);
        let val = args[0];
        let options = args[1];

        let array = match options {
            Value::Array(a) => a,
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "array".to_string(),
                    actual: options.type_name().to_string(),
                }))
            }
        };

        for opt in array.iter() {
            if *opt == *val {
                return Ok(Value::Bool(true));
            }
        }
        Ok(Value::Bool(false))
    }
}

/// validate/equals - Deep equality check
pub struct ValidateEqualsOp;
impl StrictOperator for ValidateEqualsOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/equals", >= 2);
        let val1 = args[0];
        let val2 = args[1];

        Ok(Value::Bool(deep_equals(val1, val2)))
    }
}

// ============================================================================
// Phase 4.6: Validation Extras
// ============================================================================

/// validate/phone - the `pattern` the operator table declares for it
pub struct ValidatePhoneOp;
impl StrictOperator for ValidatePhoneOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/phone", 1);
        let val = args[0];
        let val = val.text()?;
        Ok(Value::Bool(matches_semantic_pattern(
            "validate/phone",
            val,
        )?))
    }
}

/// validate/creditCard - Luhn algorithm check
pub struct ValidateCreditCardOp;
impl StrictOperator for ValidateCreditCardOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/creditCard", 1);
        let val = args[0];
        let val = val.text()?;

        // Extract only digits
        let digits: Vec<u32> = val
            .chars()
            .filter(|c| c.is_ascii_digit())
            .filter_map(|c| c.to_digit(10))
            .collect();

        // Must be 13-19 digits
        if digits.len() < 13 || digits.len() > 19 {
            return Ok(Value::Bool(false));
        }

        // Luhn algorithm
        let mut sum = 0;
        let mut double = false;

        for &digit in digits.iter().rev() {
            let mut d = digit;
            if double {
                d *= 2;
                if d > 9 {
                    d -= 9;
                }
            }
            sum += d;
            double = !double;
        }

        Ok(Value::Bool(sum % 10 == 0))
    }
}

/// validate/noneOf - Value not in set
pub struct ValidateNoneOfOp;
impl StrictOperator for ValidateNoneOfOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/noneOf", >= 2);
        let val = args[0];
        let excluded = args[1];

        let array = match excluded {
            Value::Array(a) => a,
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "array".to_string(),
                    actual: excluded.type_name().to_string(),
                }))
            }
        };

        for opt in array.iter() {
            if *opt == *val {
                return Ok(Value::Bool(false));
            }
        }
        Ok(Value::Bool(true))
    }
}

/// One field's rules: each is a rule name and its arguments.
pub(super) type FieldRules = Vec<(String, Vec<Value>)>;

/// The rules of one field: each a record `{ rule, args? }`, `rule` the rule's name and `args` its arguments.
pub(super) fn rule_list(field: &str, list: &[Value]) -> Result<FieldRules, OrbitalError> {
    let broken = |actual: &str| {
        OrbitalError::Eval(EvalError::TypeMismatch {
            expected: format!("a rule `{{ rule, args? }}` for field `{field}`"),
            actual: actual.to_string(),
        })
    };
    list.iter()
        .map(|rule| {
            let record = rule.as_object().ok_or_else(|| broken(rule.type_name()))?;
            let name = match record.get("rule") {
                Some(Value::String(name)) => name.to_string(),
                _ => return Err(broken("a record without a rule name")),
            };
            let args = match record.get("args") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(args)) => args.to_vec(),
                Some(other) => return Err(broken(&format!("args that are {}", other.type_name()))),
            };
            Ok((name, args))
        })
        .collect()
}

/// The rules argument of `validate/check`: a record of field to rule list.
pub(super) fn rules_by_field(rules: &Value) -> Result<Vec<(String, FieldRules)>, OrbitalError> {
    let mismatch = |actual: &str| {
        OrbitalError::Eval(EvalError::TypeMismatch {
            expected: "rules by field (an object of rule lists)".to_string(),
            actual: actual.to_string(),
        })
    };
    match rules {
        Value::Object(fields) => fields
            .iter()
            .map(|(field, list)| match list {
                Value::Array(rules) => Ok((field.to_string(), rule_list(field, rules)?)),
                other => Err(mismatch(other.type_name())),
            })
            .collect(),
        other => Err(mismatch(other.type_name())),
    }
}

pub(super) fn rule_holds(
    name: &str,
    value: &Value,
    args: &[Value],
) -> Result<Option<bool>, OrbitalError> {
    let arg = |at: usize| {
        args.get(at).ok_or_else(|| {
            OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "validate/check: the rule `{name}` is missing its argument {}",
                at + 1
            )))
        })
    };
    let number = |at: usize| -> Result<f64, OrbitalError> { Ok(arg(at)?.number()?) };
    let value_rule = matches!(
        name,
        "email" | "minLength" | "maxLength" | "min" | "max" | "range" | "pattern" | "oneOf"
    );
    if value_rule && matches!(value, Value::Null) {
        return Ok(Some(true));
    }
    let holds = match name {
        "required" => {
            !matches!(value, Value::Null)
                && !matches!(value, Value::String(text) if text.is_empty())
                && !matches!(value, Value::Bytes(data) if data.is_empty())
        }
        "string" => matches!(value, Value::String(_)),
        "number" => matches!(value, Value::Int(_)) || matches!(value, Value::Number(n) if !n.is_nan()),
        "boolean" => matches!(value, Value::Bool(_)),
        "email" => matches_semantic_pattern("validate/email", value.text()?)?,
        "minLength" => length_of(value)? as f64 >= number(0)?,
        "maxLength" => length_of(value)? as f64 <= number(0)?,
        "min" => value.number()? >= number(0)?,
        "max" => value.number()? <= number(0)?,
        "range" => {
            let n = value.number()?;
            n >= number(0)? && n <= number(1)?
        }
        "pattern" => {
            let text = value.text()?;
            super::string::compile_shared_regex(arg(0)?.text()?).is_some_and(|re| re.is_match(text))
        }
        "oneOf" => {
            let options = arg(0)?;
            options
                .as_array()
                .ok_or_else(|| options.kind_error("array"))?
                .iter()
                .any(|opt| opt == value)
        }
        _ => return Ok(None),
    };
    Ok(Some(holds))
}

/// validate/check - run rules by field over a value: `{ valid, errors }`, one `field: rule validation failed`
/// per broken rule, in rule order; a rule the operator does not know is skipped. A value that is not an object, or a rule
/// applied to a present value or argument of the wrong kind, is a type mismatch; an absent or null field skips the
/// value rules (`required` owns presence).
pub struct ValidateCheckOp;
impl StrictOperator for ValidateCheckOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/check", >= 2);
        let value = args[0];
        let object = value
            .as_object()
            .ok_or_else(|| value.kind_error("object"))?;
        let mut errors = Vec::new();
        for (field, rules) in rules_by_field(args[1])? {
            let field_value = object.get(&field).cloned().unwrap_or(Value::Null);
            for (name, rule_args) in rules {
                if rule_holds(&name, &field_value, &rule_args)? == Some(false) {
                    errors.push(Value::string(format!("{field}: {name} validation failed")));
                }
            }
        }
        let mut out = ValueMap::default();
        out.insert("valid".into(), Value::Bool(errors.is_empty()));
        out.insert("errors".into(), Value::array(errors));
        Ok(Value::object(out))
    }
}

// ============================================================================
// Validation value constraints (Phase 5)
// ============================================================================

/// validate/max - Maximum value constraint
pub struct ValidateMaxOp;
impl StrictOperator for ValidateMaxOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/max", >= 2);
        let num = args[0].number()?;
        let max = require_number!(args[1]);
        Ok(Value::Bool(num <= max))
    }
}

/// validate/min - Minimum value constraint
pub struct ValidateMinOp;
impl StrictOperator for ValidateMinOp {
    fn apply(&self, args: &[&Value], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/min", >= 2);
        let num = args[0].number()?;
        let min = require_number!(args[1]);
        Ok(Value::Bool(num >= min))
    }
}
