//! Validation operators and the value constraints they check.

use super::*;

// ============================================================================
// Phase 2.6: Validation Operations
// ============================================================================

/// validate/required - Check not null/empty
pub struct ValidateRequiredOp;
impl Operator for ValidateRequiredOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/required", 1);
        let val = eval_expr(&args[0], ctx)?;
        let is_valid = match &val {
            Value::Null => false,
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            _ => true,
        };
        Ok(Value::Bool(is_valid))
    }
}

/// validate/string - Type check
pub struct ValidateStringOp;
impl Operator for ValidateStringOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/string", 1);
        let val = eval_expr(&args[0], ctx)?;
        Ok(Value::Bool(TypePredicate::String.matches_value(&val)))
    }
}

/// validate/number - Type check
pub struct ValidateNumberOp;
impl Operator for ValidateNumberOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/number", 1);
        let val = eval_expr(&args[0], ctx)?;
        Ok(Value::Bool(TypePredicate::Number.matches_value(&val)))
    }
}

/// validate/boolean - Type check
pub struct ValidateBooleanOp;
impl Operator for ValidateBooleanOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/boolean", 1);
        let val = eval_expr(&args[0], ctx)?;
        Ok(Value::Bool(TypePredicate::Boolean.matches_value(&val)))
    }
}

/// validate/array - Type check
pub struct ValidateArrayOp;
impl Operator for ValidateArrayOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/array", 1);
        let val = eval_expr(&args[0], ctx)?;
        Ok(Value::Bool(TypePredicate::Array.matches_value(&val)))
    }
}

/// validate/object - Type check
pub struct ValidateObjectOp;
impl Operator for ValidateObjectOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/object", 1);
        let val = eval_expr(&args[0], ctx)?;
        Ok(Value::Bool(TypePredicate::Object.matches_value(&val)))
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
        let pattern = orbital_core_base::language::registry::operators()
            .get(operator)
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
impl Operator for ValidateEmailOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/email", 1);
        let val = eval_expr(&args[0], ctx)?.to_string_value();
        Ok(Value::Bool(matches_semantic_pattern(
            "validate/email",
            &val,
        )?))
    }
}

/// validate/url - the `pattern` the operator table declares for it
pub struct ValidateUrlOp;
impl Operator for ValidateUrlOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/url", 1);
        let val = eval_expr(&args[0], ctx)?.to_string_value();
        Ok(Value::Bool(matches_semantic_pattern("validate/url", &val)?))
    }
}

/// validate/minLength - Minimum string length
pub struct ValidateMinLengthOp;
impl Operator for ValidateMinLengthOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/minLength", >= 2);
        let val = eval_expr(&args[0], ctx)?.to_string_value();
        let min = require_number!(eval_expr(&args[1], ctx)?) as usize;

        Ok(Value::Bool(val.chars().count() >= min))
    }
}

/// validate/maxLength - Maximum string length
pub struct ValidateMaxLengthOp;
impl Operator for ValidateMaxLengthOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/maxLength", >= 2);
        let val = eval_expr(&args[0], ctx)?.to_string_value();
        let max = require_number!(eval_expr(&args[1], ctx)?) as usize;

        Ok(Value::Bool(val.chars().count() <= max))
    }
}

/// validate/pattern - Match a regex pattern of the shared dialect (see `translate_shared_regex`).
/// A non-string value, or an invalid or unsupported pattern, is false.
pub struct ValidatePatternOp;
impl Operator for ValidatePatternOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/pattern", >= 2);
        let val = eval_expr(&args[0], ctx)?;
        let pattern = eval_expr(&args[1], ctx)?;
        let (Value::String(val), Value::String(pattern)) = (val, pattern) else {
            return Ok(Value::Bool(false));
        };
        Ok(Value::Bool(
            super::string::compile_shared_regex(&pattern).is_some_and(|re| re.is_match(&val)),
        ))
    }
}

// ============================================================================
// Phase 3.6: Validation Advanced
// ============================================================================

/// validate/uuid - the `pattern` the operator table declares for it
pub struct ValidateUuidOp;
impl Operator for ValidateUuidOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/uuid", 1);
        let val = eval_expr(&args[0], ctx)?.to_string_value();
        Ok(Value::Bool(matches_semantic_pattern(
            "validate/uuid",
            &val,
        )?))
    }
}

/// validate/date - Valid date string
pub struct ValidateDateOp;
impl Operator for ValidateDateOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/date", 1);
        let val = eval_expr(&args[0], ctx)?.to_string_value();

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
impl Operator for ValidateLengthOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/length", >= 2);
        let val = eval_expr(&args[0], ctx)?;
        let expected_len = require_number!(eval_expr(&args[1], ctx)?) as usize;

        let actual_len = match &val {
            Value::String(s) => s.chars().count(),
            Value::Array(a) => a.len(),
            _ => val.to_string_value().chars().count(),
        };

        Ok(Value::Bool(actual_len == expected_len))
    }
}

/// validate/range - Value in numeric range
pub struct ValidateRangeOp;
impl Operator for ValidateRangeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/range", >= 3);
        let val = require_number!(eval_expr(&args[0], ctx)?);
        let min = require_number!(eval_expr(&args[1], ctx)?);
        let max = require_number!(eval_expr(&args[2], ctx)?);

        Ok(Value::Bool(val >= min && val <= max))
    }
}

/// validate/oneOf - Value in set
pub struct ValidateOneOfOp;
impl Operator for ValidateOneOfOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/oneOf", >= 2);
        let val = eval_expr(&args[0], ctx)?;
        let options = eval_expr(&args[1], ctx)?;

        let array = match options {
            Value::Array(a) => a,
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "array".to_string(),
                    actual: options.type_name().to_string(),
                }))
            }
        };

        let is_valid = array.iter().any(|opt| opt.loose_eq(&val));
        Ok(Value::Bool(is_valid))
    }
}

/// validate/equals - Deep equality check
pub struct ValidateEqualsOp;
impl Operator for ValidateEqualsOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/equals", >= 2);
        let val1 = eval_expr(&args[0], ctx)?;
        let val2 = eval_expr(&args[1], ctx)?;

        Ok(Value::Bool(deep_equals(&val1, &val2)))
    }
}

// ============================================================================
// Phase 4.6: Validation Extras
// ============================================================================

/// validate/phone - the `pattern` the operator table declares for it
pub struct ValidatePhoneOp;
impl Operator for ValidatePhoneOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/phone", 1);
        let val = eval_expr(&args[0], ctx)?.to_string_value();
        Ok(Value::Bool(matches_semantic_pattern(
            "validate/phone",
            &val,
        )?))
    }
}

/// validate/creditCard - Luhn algorithm check
pub struct ValidateCreditCardOp;
impl Operator for ValidateCreditCardOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/creditCard", 1);
        let val = eval_expr(&args[0], ctx)?.to_string_value();

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
impl Operator for ValidateNoneOfOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/noneOf", >= 2);
        let val = eval_expr(&args[0], ctx)?;
        let excluded = eval_expr(&args[1], ctx)?;

        let array = match excluded {
            Value::Array(a) => a,
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "array".to_string(),
                    actual: excluded.type_name().to_string(),
                }))
            }
        };

        let is_valid = !array.iter().any(|opt| opt.loose_eq(&val));
        Ok(Value::Bool(is_valid))
    }
}

/// One field's rules: each is a rule name and its evaluated arguments.
pub(super) type FieldRules = Vec<(String, Vec<Value>)>;

pub(super) fn rule_list(field: &str, list: Vec<Value>) -> Result<FieldRules, OrbitalError> {
    let broken = |actual: &str| {
        OrbitalError::Eval(EvalError::TypeMismatch {
            expected: format!("a rule `[name, args…]` for field `{field}`"),
            actual: actual.to_string(),
        })
    };
    list.into_iter()
        .map(|rule| match rule {
            Value::Array(parts) => {
                let mut parts = parts.into_iter();
                match parts.next() {
                    Some(Value::String(name)) => Ok((name.into_inner(), parts.collect())),
                    _ => Err(broken("a list without a rule name")),
                }
            }
            other => Err(broken(other.type_name())),
        })
        .collect()
}

/// The rules argument of `validate/check`: an object of field to rule list. A literal object is read as data, so
/// a rule `["minLength", 2]` is a rule and not a call of `validate/minLength`; its rule arguments are evaluated.
pub(super) fn rules_by_field(
    expr: &SExpression,
    ctx: &BindingContext,
) -> Result<Vec<(String, FieldRules)>, OrbitalError> {
    let mismatch = |actual: &str| {
        OrbitalError::Eval(EvalError::TypeMismatch {
            expected: "rules by field (an object of rule lists)".to_string(),
            actual: actual.to_string(),
        })
    };
    if let SExpression::Object(fields) = expr {
        return fields
            .iter()
            .map(|(field, list)| {
                let SExpression::Array(rules) = list else {
                    return Err(mismatch("a field whose rules are not a list"));
                };
                let evaluated = rules
                    .iter()
                    .map(|rule| match rule {
                        SExpression::Array(parts)
                            if matches!(parts.first(), Some(SExpression::String(_))) =>
                        {
                            let mut out = vec![eval_literal_head(&parts[0])];
                            for part in &parts[1..] {
                                out.push(eval_expr(part, ctx)?);
                            }
                            Ok(Value::array(out))
                        }
                        _ => Ok(Value::Null),
                    })
                    .collect::<Result<Vec<_>, OrbitalError>>()?;
                Ok((field.clone(), rule_list(field, evaluated)?))
            })
            .collect();
    }
    match eval_expr(expr, ctx)? {
        Value::Object(fields) => fields
            .into_iter()
            .map(|(field, list)| match list {
                Value::Array(rules) => Ok((field.clone(), rule_list(&field, rules.into_inner())?)),
                other => Err(mismatch(other.type_name())),
            })
            .collect(),
        Value::Null => Ok(Vec::new()),
        other => Err(mismatch(other.type_name())),
    }
}

pub(super) fn eval_literal_head(head: &SExpression) -> Value {
    match head {
        SExpression::String(name) => Value::string(name.clone()),
        _ => Value::Null,
    }
}

pub(super) fn rule_holds(
    name: &str,
    value: &Value,
    args: &[Value],
) -> Result<Option<bool>, OrbitalError> {
    let number = |at: usize| match args.get(at) {
        Some(Value::Number(n)) => Some(*n),
        _ => None,
    };
    let length = || match value {
        Value::String(text) => Some(text.chars().count() as f64),
        Value::Array(items) => Some(items.len() as f64),
        _ => None,
    };
    let holds = match name {
        "required" => {
            !matches!(value, Value::Null)
                && !matches!(value, Value::String(text) if text.is_empty())
        }
        "string" => matches!(value, Value::String(_)),
        "number" => matches!(value, Value::Number(n) if !n.is_nan()),
        "boolean" => matches!(value, Value::Bool(_)),
        "email" => match value {
            Value::String(text) => matches_semantic_pattern("validate/email", text)?,
            _ => false,
        },
        "minLength" => length().zip(number(0)).is_some_and(|(len, min)| len >= min),
        "maxLength" => length().zip(number(0)).is_some_and(|(len, max)| len <= max),
        "min" => matches!(value, Value::Number(n) if number(0).is_some_and(|min| *n >= min)),
        "max" => matches!(value, Value::Number(n) if number(0).is_some_and(|max| *n <= max)),
        "range" => {
            matches!(value, Value::Number(n) if number(0).zip(number(1)).is_some_and(|(lo, hi)| *n >= lo && *n <= hi))
        }
        "pattern" => match (value, args.first()) {
            (Value::String(text), Some(Value::String(pattern))) => {
                super::string::compile_shared_regex(pattern).is_some_and(|re| re.is_match(text))
            }
            _ => false,
        },
        "oneOf" => {
            matches!(args.first(), Some(Value::Array(options)) if options.iter().any(|opt| opt.loose_eq(value)))
        }
        _ => return Ok(None),
    };
    Ok(Some(holds))
}

/// validate/check - run rules by field over a value: `{ valid, errors }`, one `field: rule validation failed`
/// per broken rule, in rule order; a rule the operator does not know is skipped.
pub struct ValidateCheckOp;
impl Operator for ValidateCheckOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/check", >= 2);
        let value = eval_expr(&args[0], ctx)?;
        let mut errors = Vec::new();
        for (field, rules) in rules_by_field(&args[1], ctx)? {
            let field_value = value
                .as_object()
                .and_then(|object| object.get(&field))
                .cloned()
                .unwrap_or(Value::Null);
            for (name, rule_args) in rules {
                if rule_holds(&name, &field_value, &rule_args)? == Some(false) {
                    errors.push(Value::string(format!("{field}: {name} validation failed")));
                }
            }
        }
        let mut out = ValueMap::default();
        out.insert("valid".to_string(), Value::Bool(errors.is_empty()));
        out.insert("errors".to_string(), Value::array(errors));
        Ok(Value::object(out))
    }
}

// ============================================================================
// Validation value constraints (Phase 5)
// ============================================================================

/// validate/max - Maximum value constraint
pub struct ValidateMaxOp;
impl Operator for ValidateMaxOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/max", >= 2);
        let val = eval_expr(&args[0], ctx)?;
        let max = require_number!(eval_expr(&args[1], ctx)?);
        let num = val.to_number().unwrap_or(f64::INFINITY);
        Ok(Value::Bool(num <= max))
    }
}

/// validate/min - Minimum value constraint
pub struct ValidateMinOp;
impl Operator for ValidateMinOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/min", >= 2);
        let val = eval_expr(&args[0], ctx)?;
        let min = require_number!(eval_expr(&args[1], ctx)?);
        let num = val.to_number().unwrap_or(f64::NEG_INFINITY);
        Ok(Value::Bool(num >= min))
    }
}
