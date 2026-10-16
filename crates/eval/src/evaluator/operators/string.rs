//! String operators.

use super::macros::*;
use super::*;

// ============================================================================
// String Operators
// ============================================================================

pub struct ConcatOp;
impl Operator for ConcatOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        let mut result = String::new();
        for arg in args {
            let val = eval_expr(arg, ctx)?;
            result.push_str(&val.to_string_value());
        }
        Ok(Value::String(result))
    }
}

pub struct UpperOp;
pub struct StrToNumberOp;
impl Operator for StrToNumberOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.len() != 1 {
            return Err(OrbitalError::Eval(EvalError::InvalidArgument(
                "str/to-number: expected exactly one argument".into(),
            )));
        }
        match eval_expr(&args[0], ctx)? {
            Value::String(text) => Ok(Value::Number(orbital_core_base::str_utils::js_number_from_text(&text))),
            other => Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "string".into(),
                actual: other.type_name().into(),
            })),
        }
    }
}

impl Operator for UpperOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "upper", 1);
        let val = eval_expr(&args[0], ctx)?;
        Ok(Value::String(val.to_string_value().to_uppercase()))
    }
}

pub struct LowerOp;
impl Operator for LowerOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "lower", 1);
        let val = eval_expr(&args[0], ctx)?;
        Ok(Value::String(val.to_string_value().to_lowercase()))
    }
}

pub struct TrimOp;
impl Operator for TrimOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "trim", 1);
        let val = eval_expr(&args[0], ctx)?;
        Ok(Value::String(val.to_string_value().trim().to_string()))
    }
}

pub struct SubstringOp;
impl Operator for SubstringOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "substring", >= 2);
        let s = eval_expr(&args[0], ctx)?.to_string_value();
        let start = require_number!(eval_expr(&args[1], ctx)?) as usize;

        let end = if args.len() > 2 {
            require_number!(eval_expr(&args[2], ctx)?) as usize
        } else {
            s.len()
        };

        let chars: Vec<char> = s.chars().collect();
        let start = start.min(chars.len());
        let end = end.min(chars.len());

        Ok(Value::String(chars[start..end].iter().collect()))
    }
}

pub struct SplitOp;
impl Operator for SplitOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "split", >= 2);
        let s = eval_expr(&args[0], ctx)?.to_string_value();
        let delimiter = eval_expr(&args[1], ctx)?.to_string_value();

        let parts: Vec<Value> = s
            .split(&delimiter)
            .map(|p| Value::String(p.to_string()))
            .collect();
        Ok(Value::Array(parts))
    }
}

pub struct JoinOp;
impl Operator for JoinOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "join", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let delimiter = eval_expr(&args[1], ctx)?.to_string_value();

        let array = require_array!(arr);

        let parts: Vec<String> = array.into_iter().map(|v| v.to_string_value()).collect();
        Ok(Value::String(parts.join(&delimiter)))
    }
}

// ============================================================================
// Phase 2.2: String Operations
// ============================================================================

/// str/len - String length (characters, not bytes)
pub struct StrLenOp;
impl Operator for StrLenOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/len", 1);
        let val = eval_expr(&args[0], ctx)?;
        let s = val.to_string_value();
        Ok(Value::Number(s.chars().count() as f64))
    }
}

/// str/includes - Check if substring exists
pub struct StrIncludesOp;
impl Operator for StrIncludesOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/includes", >= 2);
        let haystack = eval_expr(&args[0], ctx)?.to_string_value();
        let needle = eval_expr(&args[1], ctx)?.to_string_value();
        Ok(Value::Bool(haystack.contains(&needle)))
    }
}

/// str/startsWith - Check prefix
pub struct StrStartsWithOp;
impl Operator for StrStartsWithOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/startsWith", >= 2);
        let s = eval_expr(&args[0], ctx)?.to_string_value();
        let prefix = eval_expr(&args[1], ctx)?.to_string_value();
        Ok(Value::Bool(s.starts_with(&prefix)))
    }
}

/// str/endsWith - Check suffix
pub struct StrEndsWithOp;
impl Operator for StrEndsWithOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/endsWith", >= 2);
        let s = eval_expr(&args[0], ctx)?.to_string_value();
        let suffix = eval_expr(&args[1], ctx)?.to_string_value();
        Ok(Value::Bool(s.ends_with(&suffix)))
    }
}

/// str/replace - Replace first occurrence
pub struct StrReplaceOp;
impl Operator for StrReplaceOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/replace", >= 3);
        let s = eval_expr(&args[0], ctx)?.to_string_value();
        let old = eval_expr(&args[1], ctx)?.to_string_value();
        let new = eval_expr(&args[2], ctx)?.to_string_value();
        Ok(Value::String(s.replacen(&old, &new, 1)))
    }
}

/// str/replaceAll - Replace all occurrences
pub struct StrReplaceAllOp;
impl Operator for StrReplaceAllOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/replaceAll", >= 3);
        let s = eval_expr(&args[0], ctx)?.to_string_value();
        let old = eval_expr(&args[1], ctx)?.to_string_value();
        let new = eval_expr(&args[2], ctx)?.to_string_value();
        Ok(Value::String(s.replace(&old, &new)))
    }
}

/// str/template - Template interpolation with {{key}} syntax
pub struct StrTemplateOp;
impl Operator for StrTemplateOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/template", >= 2);
        let template = eval_expr(&args[0], ctx)?.to_string_value();
        let data = eval_expr(&args[1], ctx)?;

        let obj = match data {
            Value::Object(o) => o,
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "object".to_string(),
                    actual: data.type_name().to_string(),
                }))
            }
        };

        // Replace {{key}} patterns
        let mut result = template.clone();
        for (key, value) in &obj {
            let placeholder = format!("{{{{{}}}}}", key);
            result = result.replace(&placeholder, &value.to_string_value());
        }

        Ok(Value::String(result))
    }
}

/// str/indexOf - Find substring index
pub struct StrIndexOfOp;
impl Operator for StrIndexOfOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/indexOf", >= 2);
        let haystack = eval_expr(&args[0], ctx)?.to_string_value();
        let needle = eval_expr(&args[1], ctx)?.to_string_value();

        match haystack.find(&needle) {
            Some(pos) => {
                // Convert byte index to char index
                let char_pos = haystack[..pos].chars().count();
                Ok(Value::Number(char_pos as f64))
            }
            None => Ok(Value::Number(-1.0)),
        }
    }
}

// ============================================================================
// Phase 3.2: String Advanced
// ============================================================================

/// str/slice - Extract by indices
pub struct StrSliceOp;
impl Operator for StrSliceOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/slice", >= 2);
        let s = eval_expr(&args[0], ctx)?.to_string_value();
        let start = require_number!(eval_expr(&args[1], ctx)?);

        let chars: Vec<char> = s.chars().collect();
        let len = chars.len() as i64;

        let start_idx = if start < 0.0 {
            ((len + start as i64).max(0)) as usize
        } else {
            (start as usize).min(chars.len())
        };

        let end_idx = if args.len() > 2 {
            let end = require_number!(eval_expr(&args[2], ctx)?);
            if end < 0.0 {
                ((len + end as i64).max(0)) as usize
            } else {
                (end as usize).min(chars.len())
            }
        } else {
            chars.len()
        };

        if start_idx >= end_idx {
            return Ok(Value::String(String::new()));
        }

        Ok(Value::String(chars[start_idx..end_idx].iter().collect()))
    }
}

/// str/padStart - Pad start to length
pub struct StrPadStartOp;
impl Operator for StrPadStartOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "padStart", >= 2);
        let s = eval_expr(&args[0], ctx)?.to_string_value();
        let target_len = require_number!(eval_expr(&args[1], ctx)?) as usize;

        let pad_char = if args.len() > 2 {
            let pad_str = eval_expr(&args[2], ctx)?.to_string_value();
            pad_str.chars().next().unwrap_or(' ')
        } else {
            ' '
        };

        let current_len = s.chars().count();
        if current_len >= target_len {
            return Ok(Value::String(s));
        }

        let padding: String = std::iter::repeat_n(pad_char, target_len - current_len).collect();
        Ok(Value::String(format!("{}{}", padding, s)))
    }
}

/// str/padEnd - Pad end to length
pub struct StrPadEndOp;
impl Operator for StrPadEndOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "padEnd", >= 2);
        let s = eval_expr(&args[0], ctx)?.to_string_value();
        let target_len = require_number!(eval_expr(&args[1], ctx)?) as usize;

        let pad_char = if args.len() > 2 {
            let pad_str = eval_expr(&args[2], ctx)?.to_string_value();
            pad_str.chars().next().unwrap_or(' ')
        } else {
            ' '
        };

        let current_len = s.chars().count();
        if current_len >= target_len {
            return Ok(Value::String(s));
        }

        let padding: String = std::iter::repeat_n(pad_char, target_len - current_len).collect();
        Ok(Value::String(format!("{}{}", s, padding)))
    }
}

/// str/capitalize - Capitalize first letter
pub struct StrCapitalizeOp;
impl Operator for StrCapitalizeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "capitalize", 1);
        let s = eval_expr(&args[0], ctx)?.to_string_value();

        if s.is_empty() {
            return Ok(Value::String(s));
        }

        let mut chars = s.chars();
        let first = chars.next().unwrap().to_uppercase().to_string();
        let rest: String = chars.collect::<String>().to_lowercase();

        Ok(Value::String(format!("{}{}", first, rest)))
    }
}

/// str/truncate - Truncate with suffix
/// Truncates to max_len characters and appends suffix (suffix is extra, not counted in max_len)
pub struct StrTruncateOp;
impl Operator for StrTruncateOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "truncate", >= 2);
        let s = eval_expr(&args[0], ctx)?.to_string_value();
        let max_len = require_number!(eval_expr(&args[1], ctx)?) as usize;

        let suffix = if args.len() > 2 {
            eval_expr(&args[2], ctx)?.to_string_value()
        } else {
            "...".to_string()
        };

        let char_count = s.chars().count();
        if char_count <= max_len {
            return Ok(Value::String(s));
        }

        // Truncate at max_len and append suffix
        let truncated: String = s.chars().take(max_len).collect();
        Ok(Value::String(format!("{}{}", truncated, suffix)))
    }
}

// ============================================================================
// Phase 4.2: String Extras
// ============================================================================

/// str/trimStart - Trim leading whitespace
pub struct StrTrimStartOp;
impl Operator for StrTrimStartOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "trimStart", 1);
        let s = eval_expr(&args[0], ctx)?.to_string_value();
        Ok(Value::String(s.trim_start().to_string()))
    }
}

/// str/trimEnd - Trim trailing whitespace
pub struct StrTrimEndOp;
impl Operator for StrTrimEndOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "trimEnd", 1);
        let s = eval_expr(&args[0], ctx)?.to_string_value();
        Ok(Value::String(s.trim_end().to_string()))
    }
}

/// str/repeat - Repeat string n times
pub struct StrRepeatOp;
impl Operator for StrRepeatOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/repeat", >= 2);
        let s = eval_expr(&args[0], ctx)?.to_string_value();
        let n = require_number!(eval_expr(&args[1], ctx)?) as usize;

        Ok(Value::String(s.repeat(n)))
    }
}

/// str/reverse - Reverse string
pub struct StrReverseOp;
impl Operator for StrReverseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/reverse", 1);
        let s = eval_expr(&args[0], ctx)?.to_string_value();
        Ok(Value::String(s.chars().rev().collect()))
    }
}

/// str/titleCase - Title Case Each Word
pub struct StrTitleCaseOp;
impl Operator for StrTitleCaseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "titleCase", 1);
        let s = eval_expr(&args[0], ctx)?.to_string_value();

        let result: String = s
            .split_whitespace()
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => {
                        first.to_uppercase().to_string() + &chars.collect::<String>().to_lowercase()
                    }
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        Ok(Value::String(result))
    }
}

/// str/camelCase - Convert to camelCase
pub struct StrCamelCaseOp;
impl Operator for StrCamelCaseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "camelCase", 1);
        let s = eval_expr(&args[0], ctx)?.to_string_value();

        // Split on non-alphanumeric characters
        let words: Vec<&str> = s
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .collect();

        let result: String = words
            .iter()
            .enumerate()
            .map(|(i, word)| {
                if i == 0 {
                    word.to_lowercase()
                } else {
                    let mut chars = word.chars();
                    match chars.next() {
                        None => String::new(),
                        Some(first) => {
                            first.to_uppercase().to_string()
                                + &chars.collect::<String>().to_lowercase()
                        }
                    }
                }
            })
            .collect();

        Ok(Value::String(result))
    }
}

/// str/kebabCase - Convert to kebab-case
pub struct StrKebabCaseOp;
impl Operator for StrKebabCaseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "kebabCase", 1);
        let s = eval_expr(&args[0], ctx)?.to_string_value();

        // Split on non-alphanumeric characters or camelCase boundaries
        let mut words = Vec::new();
        let mut current_word = String::new();

        for c in s.chars() {
            if c.is_alphanumeric() {
                if c.is_uppercase() && !current_word.is_empty() {
                    words.push(current_word.to_lowercase());
                    current_word = String::new();
                }
                current_word.push(c);
            } else if !current_word.is_empty() {
                words.push(current_word.to_lowercase());
                current_word = String::new();
            }
        }
        if !current_word.is_empty() {
            words.push(current_word.to_lowercase());
        }

        Ok(Value::String(words.join("-")))
    }
}

/// str/snakeCase - Convert to snake_case
pub struct StrSnakeCaseOp;
impl Operator for StrSnakeCaseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "snakeCase", 1);
        let s = eval_expr(&args[0], ctx)?.to_string_value();

        // Split on non-alphanumeric characters or camelCase boundaries
        let mut words = Vec::new();
        let mut current_word = String::new();

        for c in s.chars() {
            if c.is_alphanumeric() {
                if c.is_uppercase() && !current_word.is_empty() {
                    words.push(current_word.to_lowercase());
                    current_word = String::new();
                }
                current_word.push(c);
            } else if !current_word.is_empty() {
                words.push(current_word.to_lowercase());
                current_word = String::new();
            }
        }
        if !current_word.is_empty() {
            words.push(current_word.to_lowercase());
        }

        Ok(Value::String(words.join("_")))
    }
}

/// str/default - Default value if empty
pub struct StrDefaultOp;
impl Operator for StrDefaultOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/default", >= 2);
        let val = eval_expr(&args[0], ctx)?;
        let fallback = eval_expr(&args[1], ctx)?;

        match &val {
            Value::Null => Ok(fallback),
            Value::String(s) if s.is_empty() => Ok(fallback),
            _ => Ok(val),
        }
    }
}
