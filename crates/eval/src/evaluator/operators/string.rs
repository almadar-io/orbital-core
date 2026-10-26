//! String operators.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

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
            result.push_str(&val.to_text());
        }
        Ok(Value::string(result))
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
            Value::String(text) => {
                let n = orbital_core_base::str_utils::js_number_from_text(&text);
                Ok(if n.is_nan() { Value::Null } else { Value::Number(n) })
            }
            other => Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "string".into(),
                actual: other.type_name().into(),
            })),
        }
    }
}

/// str/to-int - Rust `i64::from_str`: an optional `+` or `-`, then one or more ASCII digits, nothing else; null for
/// any other text and for a value beyond 2^53 - 1 either way.
pub struct StrToIntOp;
impl Operator for StrToIntOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/to-int", == 1);
        match eval_expr(&args[0], ctx)? {
            Value::String(text) => Ok(text
                .parse::<i64>()
                .ok()
                .filter(|n| n.unsigned_abs() <= (1u64 << 53) - 1)
                .map_or(Value::Null, |n| Value::Number(n as f64))),
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
        Ok(Value::string(val.to_text().to_uppercase()))
    }
}

pub struct LowerOp;
impl Operator for LowerOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "lower", 1);
        let val = eval_expr(&args[0], ctx)?;
        Ok(Value::string(val.to_text().to_lowercase()))
    }
}

pub struct TrimOp;
impl Operator for TrimOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "trim", 1);
        let val = eval_expr(&args[0], ctx)?;
        Ok(Value::string(val.to_text().trim().to_string()))
    }
}

pub struct SubstringOp;
impl Operator for SubstringOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "substring", >= 2);
        let s = eval_expr(&args[0], ctx)?.to_text();
        let start = require_number!(eval_expr(&args[1], ctx)?) as usize;

        let end = if args.len() > 2 {
            require_number!(eval_expr(&args[2], ctx)?) as usize
        } else {
            s.len()
        };

        let chars: Vec<char> = s.chars().collect();
        let start = start.min(chars.len());
        let end = end.min(chars.len()).max(start);

        Ok(Value::String(chars[start..end].iter().collect()))
    }
}

pub struct SplitOp;
impl Operator for SplitOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "split", >= 2);
        let s = eval_expr(&args[0], ctx)?.to_text();
        let delimiter = eval_expr(&args[1], ctx)?.to_text();

        let parts: Vec<Value> = if delimiter.is_empty() {
            s.chars().map(|c| Value::string(c.to_string())).collect()
        } else {
            s.split(&delimiter)
                .map(|p| Value::string(p.to_string()))
                .collect()
        };
        Ok(Value::array(parts))
    }
}

pub struct JoinOp;
impl Operator for JoinOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "join", >= 2);
        let arr = eval_expr(&args[0], ctx)?;
        let delimiter = eval_expr(&args[1], ctx)?.to_text();

        let array = require_array!(arr);

        let parts: Vec<String> = array.into_iter().map(|v| v.to_text()).collect();
        Ok(Value::string(parts.join(&delimiter)))
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
        let s = val.to_text();
        Ok(Value::Number(s.chars().count() as f64))
    }
}

/// str/includes - Check if substring exists
pub struct StrIncludesOp;
impl Operator for StrIncludesOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/includes", >= 2);
        let haystack = eval_expr(&args[0], ctx)?.to_text();
        let needle = eval_expr(&args[1], ctx)?.to_text();
        Ok(Value::Bool(haystack.contains(&needle)))
    }
}

/// str/startsWith - Check prefix
pub struct StrStartsWithOp;
impl Operator for StrStartsWithOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/startsWith", >= 2);
        let s = eval_expr(&args[0], ctx)?.to_text();
        let prefix = eval_expr(&args[1], ctx)?.to_text();
        Ok(Value::Bool(s.starts_with(&prefix)))
    }
}

/// str/endsWith - Check suffix
pub struct StrEndsWithOp;
impl Operator for StrEndsWithOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/endsWith", >= 2);
        let s = eval_expr(&args[0], ctx)?.to_text();
        let suffix = eval_expr(&args[1], ctx)?.to_text();
        Ok(Value::Bool(s.ends_with(&suffix)))
    }
}

/// str/replace - Replace first occurrence
pub struct StrReplaceOp;
impl Operator for StrReplaceOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/replace", >= 3);
        let s = eval_expr(&args[0], ctx)?.to_text();
        let old = eval_expr(&args[1], ctx)?.to_text();
        let new = eval_expr(&args[2], ctx)?.to_text();
        Ok(Value::string(s.replacen(&old, &new, 1)))
    }
}

/// str/replace-pattern - Replace every match of a shared-dialect regex (see `translate_shared_regex`).
/// The replacement is literal (`$` has no meaning); an invalid or unsupported pattern answers null.
pub struct StrReplacePatternOp;
impl Operator for StrReplacePatternOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/replace-pattern", >= 3);
        let mut texts = Vec::with_capacity(3);
        for arg in &args[..3] {
            match eval_expr(arg, ctx)? {
                Value::String(text) => texts.push(text),
                other => {
                    return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                        expected: "string".into(),
                        actual: other.type_name().into(),
                    }))
                }
            }
        }
        let Some(regex) = compile_shared_regex(&texts[1]) else {
            return Ok(Value::Null);
        };
        Ok(Value::string(replace_each_match(
            &regex, &texts[0], &texts[2],
        )))
    }
}

/// str/replaceAll - Replace all occurrences
pub struct StrReplaceAllOp;
impl Operator for StrReplaceAllOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/replaceAll", >= 3);
        let s = eval_expr(&args[0], ctx)?.to_text();
        let old = eval_expr(&args[1], ctx)?.to_text();
        let new = eval_expr(&args[2], ctx)?.to_text();
        Ok(Value::string(s.replace(&old, &new)))
    }
}

/// str/template - Template interpolation with {{key}} syntax
pub struct StrTemplateOp;
impl Operator for StrTemplateOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/template", >= 2);
        let template = eval_expr(&args[0], ctx)?.to_text();
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
            result = result.replace(&placeholder, &value.to_text());
        }

        Ok(Value::string(result))
    }
}

/// str/indexOf - Find substring index
pub struct StrIndexOfOp;
impl Operator for StrIndexOfOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/indexOf", >= 2);
        let haystack = eval_expr(&args[0], ctx)?.to_text();
        let needle = eval_expr(&args[1], ctx)?.to_text();

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
        let s = eval_expr(&args[0], ctx)?.to_text();
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
            return Ok(Value::string(String::new()));
        }

        Ok(Value::String(chars[start_idx..end_idx].iter().collect()))
    }
}

/// str/padStart - Pad start to length
pub struct StrPadStartOp;
impl Operator for StrPadStartOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "padStart", >= 2);
        let s = eval_expr(&args[0], ctx)?.to_text();
        let target_len = require_number!(eval_expr(&args[1], ctx)?) as usize;

        let pad_char = if args.len() > 2 {
            let pad_str = eval_expr(&args[2], ctx)?.to_text();
            pad_str.chars().next().unwrap_or(' ')
        } else {
            ' '
        };

        let current_len = s.chars().count();
        if current_len >= target_len {
            return Ok(Value::string(s));
        }

        let padding: String = std::iter::repeat_n(pad_char, target_len - current_len).collect();
        Ok(Value::string(format!("{}{}", padding, s)))
    }
}

/// str/padEnd - Pad end to length
pub struct StrPadEndOp;
impl Operator for StrPadEndOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "padEnd", >= 2);
        let s = eval_expr(&args[0], ctx)?.to_text();
        let target_len = require_number!(eval_expr(&args[1], ctx)?) as usize;

        let pad_char = if args.len() > 2 {
            let pad_str = eval_expr(&args[2], ctx)?.to_text();
            pad_str.chars().next().unwrap_or(' ')
        } else {
            ' '
        };

        let current_len = s.chars().count();
        if current_len >= target_len {
            return Ok(Value::string(s));
        }

        let padding: String = std::iter::repeat_n(pad_char, target_len - current_len).collect();
        Ok(Value::string(format!("{}{}", s, padding)))
    }
}

/// str/capitalize - Capitalize first letter
pub struct StrCapitalizeOp;
impl Operator for StrCapitalizeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "capitalize", 1);
        let s = eval_expr(&args[0], ctx)?.to_text();

        if s.is_empty() {
            return Ok(Value::string(s));
        }

        let mut chars = s.chars();
        let first = chars.next().unwrap().to_uppercase().to_string();
        let rest: String = chars.collect::<String>().to_lowercase();

        Ok(Value::string(format!("{}{}", first, rest)))
    }
}

/// str/truncate - Truncate with suffix
/// Truncates to max_len characters and appends suffix (suffix is extra, not counted in max_len)
pub struct StrTruncateOp;
impl Operator for StrTruncateOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "truncate", >= 2);
        let s = eval_expr(&args[0], ctx)?.to_text();
        let max_len = require_number!(eval_expr(&args[1], ctx)?) as usize;

        let suffix = if args.len() > 2 {
            eval_expr(&args[2], ctx)?.to_text()
        } else {
            "...".to_string()
        };

        let char_count = s.chars().count();
        if char_count <= max_len {
            return Ok(Value::string(s));
        }

        // Truncate at max_len and append suffix
        let truncated: String = s.chars().take(max_len).collect();
        Ok(Value::string(format!("{}{}", truncated, suffix)))
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
        let s = eval_expr(&args[0], ctx)?.to_text();
        Ok(Value::string(s.trim_start().to_string()))
    }
}

/// str/trimEnd - Trim trailing whitespace
pub struct StrTrimEndOp;
impl Operator for StrTrimEndOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "trimEnd", 1);
        let s = eval_expr(&args[0], ctx)?.to_text();
        Ok(Value::string(s.trim_end().to_string()))
    }
}

/// str/repeat - Repeat string n times
pub struct StrRepeatOp;
impl Operator for StrRepeatOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/repeat", >= 2);
        let s = eval_expr(&args[0], ctx)?.to_text();
        let n = require_number!(eval_expr(&args[1], ctx)?) as usize;

        Ok(Value::string(s.repeat(n)))
    }
}

/// str/reverse - Reverse string
pub struct StrReverseOp;
impl Operator for StrReverseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/reverse", 1);
        let s = eval_expr(&args[0], ctx)?.to_text();
        Ok(Value::String(s.chars().rev().collect()))
    }
}

/// str/titleCase - Title Case Each Word
pub struct StrTitleCaseOp;
impl Operator for StrTitleCaseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "titleCase", 1);
        let s = eval_expr(&args[0], ctx)?.to_text();

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

        Ok(Value::string(result))
    }
}

/// str/camelCase - Convert to camelCase
pub struct StrCamelCaseOp;
impl Operator for StrCamelCaseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "camelCase", 1);
        let s = eval_expr(&args[0], ctx)?.to_text();

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

        Ok(Value::string(result))
    }
}

/// str/kebabCase - Convert to kebab-case
pub struct StrKebabCaseOp;
impl Operator for StrKebabCaseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "kebabCase", 1);
        let s = eval_expr(&args[0], ctx)?.to_text();

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

        Ok(Value::string(words.join("-")))
    }
}

/// str/snakeCase - Convert to snake_case
pub struct StrSnakeCaseOp;
impl Operator for StrSnakeCaseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "snakeCase", 1);
        let s = eval_expr(&args[0], ctx)?.to_text();

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

        Ok(Value::string(words.join("_")))
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

// ============================================================================
// Unicode collation (CLDR root locale, compiled data: the same bytes on every target)
// ============================================================================

fn root_collator() -> icu_collator::CollatorBorrowed<'static> {
    icu_collator::CollatorBorrowed::new_root(icu_collator::options::CollatorOptions::default())
}

fn collate_text(
    arg: &SExpression,
    ctx: &BindingContext,
    op: &str,
) -> Result<Option<String>, OrbitalError> {
    match eval_expr(arg, ctx)? {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.into_inner())),
        other => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
            "{op}: expected a string argument, got {}",
            other.type_name()
        )))),
    }
}

/// str/collate - -1, 0 or 1 by the CLDR root collation (tertiary strength); null on either side is null
pub struct StrCollateOp;
impl Operator for StrCollateOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/collate", == 2);
        let (Some(a), Some(b)) = (
            collate_text(&args[0], ctx, "str/collate")?,
            collate_text(&args[1], ctx, "str/collate")?,
        ) else {
            return Ok(Value::Null);
        };
        Ok(Value::Number(match root_collator().compare(&a, &b) {
            std::cmp::Ordering::Less => -1.0,
            std::cmp::Ordering::Equal => 0.0,
            std::cmp::Ordering::Greater => 1.0,
        }))
    }
}

/// str/collate-key - a string whose code-point order is the root collation order of its argument (the collation
/// sort key in lowercase hex), so `array/sort` by a field holding it sorts by collation; null is null
pub struct StrCollateKeyOp;
impl Operator for StrCollateKeyOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "str/collate-key", == 1);
        let Some(text) = collate_text(&args[0], ctx, "str/collate-key")? else {
            return Ok(Value::Null);
        };
        let mut key: Vec<u8> = Vec::new();
        let Ok(()) = root_collator().write_sort_key_to(&text, &mut key);
        Ok(Value::string(orbital_core_base::codec::hex(&key)))
    }
}

// ============================================================================
// Shared regular-expression dialect
// ============================================================================

const REGEX_SYNTAX_ESCAPES: &str = "\\^$.*+?()[]{}|/";
const REGEX_GENERAL_CATEGORIES: &[&str] = &[
    "L", "Lu", "Ll", "Lt", "Lm", "Lo", "M", "Mn", "Mc", "Me", "N", "Nd", "Nl", "No", "P", "Pc",
    "Pd", "Ps", "Pe", "Pi", "Pf", "Po", "S", "Sm", "Sc", "Sk", "So", "Z", "Zs", "Zl", "Zp", "Cc",
    "Cf", "Co",
];
const REGEX_MAX_REPEAT: usize = 1000;
const REGEX_JS_SPACE: &str = r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";

fn regex_control_escape(e: char) -> Option<char> {
    match e {
        'n' => Some('\n'),
        'r' => Some('\r'),
        't' => Some('\t'),
        'f' => Some('\u{0C}'),
        'v' => Some('\u{0B}'),
        _ => None,
    }
}

fn regex_hex_at(chars: &[char], at: usize, len: usize) -> Option<u32> {
    let digits = chars.get(at..at + len)?;
    if !digits.iter().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(&digits.iter().collect::<String>(), 16).ok()
}

/// Parses `\p{Name}` / `\P{Name}` where `at` is the letter after the backslash; returns the next index.
fn regex_property_end(chars: &[char], at: usize) -> Option<usize> {
    if chars.get(at + 1) != Some(&'{') {
        return None;
    }
    let close = at + 2 + chars.get(at + 2..)?.iter().position(|c| *c == '}')?;
    let name: String = chars[at + 2..close].iter().collect();
    REGEX_GENERAL_CATEGORIES
        .contains(&name.as_str())
        .then_some(close + 1)
}

enum ClassAtom {
    Char(u32),
    Set(String),
}

/// One class member at `at`: its atom and the next index.
fn regex_class_atom(chars: &[char], at: usize) -> Option<(ClassAtom, usize)> {
    let c = *chars.get(at)?;
    if c == '[' || c == ']' {
        return None;
    }
    if c != '\\' {
        if matches!(c, '&' | '~' | '-') && chars.get(at + 1) == Some(&c) {
            return None;
        }
        return Some((ClassAtom::Char(c as u32), at + 1));
    }
    let e = *chars.get(at + 1)?;
    match e {
        'd' => Some((ClassAtom::Set("0-9".into()), at + 2)),
        'w' => Some((ClassAtom::Set("0-9A-Za-z_".into()), at + 2)),
        's' => Some((ClassAtom::Set(REGEX_JS_SPACE.into()), at + 2)),
        'p' | 'P' => {
            let end = regex_property_end(chars, at + 1)?;
            let text: String = chars[at + 1..end].iter().collect();
            Some((ClassAtom::Set(format!("\\{text}")), end))
        }
        '-' => Some((ClassAtom::Char('-' as u32), at + 2)),
        _ if REGEX_SYNTAX_ESCAPES.contains(e) => Some((ClassAtom::Char(e as u32), at + 2)),
        _ if regex_control_escape(e).is_some() => {
            Some((ClassAtom::Char(regex_control_escape(e)? as u32), at + 2))
        }
        'x' => Some((ClassAtom::Char(regex_hex_at(chars, at + 2, 2)?), at + 4)),
        'u' => {
            let code = regex_hex_at(chars, at + 2, 4)?;
            (!(0xD800..=0xDFFF).contains(&code)).then_some((ClassAtom::Char(code), at + 6))
        }
        _ => None,
    }
}

/// A class body after `[`: its translation and the index after `]`.
fn regex_class(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut i = start;
    let mut out = String::from("[");
    if chars.get(i) == Some(&'^') {
        out.push('^');
        i += 1;
    }
    if chars.get(i) == Some(&']') {
        return None;
    }
    while i < chars.len() {
        if chars[i] == ']' {
            out.push(']');
            return Some((out, i + 1));
        }
        let (first, next) = regex_class_atom(chars, i)?;
        i = next;
        let range_follows = chars.get(i) == Some(&'-') && chars.get(i + 1) != Some(&']');
        match first {
            ClassAtom::Set(set) => {
                if range_follows {
                    return None;
                }
                out.push_str(&set);
            }
            ClassAtom::Char(low) => {
                out.push_str(&format!("\\x{{{low:X}}}"));
                if range_follows {
                    let (second, next) = regex_class_atom(chars, i + 1)?;
                    let ClassAtom::Char(high) = second else {
                        return None;
                    };
                    if low > high {
                        return None;
                    }
                    out.push_str(&format!("-\\x{{{high:X}}}"));
                    i = next;
                }
            }
        }
    }
    None
}

/// `{n}`, `{n,}` or `{n,m}` at `at`: its text and the next index.
fn regex_braces(chars: &[char], at: usize) -> Option<(String, usize)> {
    let mut i = at + 1;
    let number = |i: &mut usize| -> Option<usize> {
        let from = *i;
        while chars.get(*i).is_some_and(|c| c.is_ascii_digit()) {
            *i += 1;
        }
        if *i == from {
            return None;
        }
        chars[from..*i].iter().collect::<String>().parse().ok()
    };
    let low = number(&mut i)?;
    if low > REGEX_MAX_REPEAT {
        return None;
    }
    if chars.get(i) == Some(&',') {
        i += 1;
        if let Some(high) = number(&mut i) {
            if high > REGEX_MAX_REPEAT || high < low {
                return None;
            }
        }
    }
    if chars.get(i) != Some(&'}') {
        return None;
    }
    let text: String = chars[at..=i].iter().collect();
    Some((text, i + 1))
}

/// Translates a pattern of the shared dialect (see `validate/pattern`) into the `regex` crate's syntax with
/// identical meaning; `None` when the pattern is invalid or outside the dialect. The ECMAScript twin is
/// `isSharedRegex` in `@almadar/evaluator` `std/regex.ts`; `regex-vectors.json` pins both.
pub fn translate_shared_regex(pattern: &str) -> Option<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut depth = 0usize;
    let mut quantifiable = false;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '(' => {
                if chars.get(i + 1) == Some(&'?') {
                    if chars.get(i + 2) != Some(&':') {
                        return None;
                    }
                    out.push_str("(?:");
                    i += 3;
                } else {
                    out.push('(');
                    i += 1;
                }
                depth += 1;
                quantifiable = false;
            }
            ')' => {
                depth = depth.checked_sub(1)?;
                out.push(')');
                i += 1;
                quantifiable = true;
            }
            '|' | '^' | '$' => {
                out.push(c);
                i += 1;
                quantifiable = false;
            }
            '*' | '+' | '?' | '{' => {
                if !quantifiable {
                    return None;
                }
                if c == '{' {
                    let (text, next) = regex_braces(&chars, i)?;
                    out.push_str(&text);
                    i = next;
                } else {
                    out.push(c);
                    i += 1;
                }
                if chars.get(i) == Some(&'?') {
                    out.push('?');
                    i += 1;
                }
                quantifiable = false;
            }
            '}' | ']' => return None,
            '.' => {
                out.push_str(r"[^\n\r\x{2028}\x{2029}]");
                i += 1;
                quantifiable = true;
            }
            '[' => {
                let (text, next) = regex_class(&chars, i + 1)?;
                out.push_str(&text);
                i = next;
                quantifiable = true;
            }
            '\\' => {
                let e = *chars.get(i + 1)?;
                match e {
                    'b' => out.push_str(r"(?-u:\b)"),
                    'B' => out.push_str(r"(?-u:\B)"),
                    'd' => out.push_str("[0-9]"),
                    'D' => out.push_str("[^0-9]"),
                    'w' => out.push_str("[0-9A-Za-z_]"),
                    'W' => out.push_str("[^0-9A-Za-z_]"),
                    's' => out.push_str(&format!("[{REGEX_JS_SPACE}]")),
                    'S' => out.push_str(&format!("[^{REGEX_JS_SPACE}]")),
                    'p' | 'P' => {
                        let end = regex_property_end(&chars, i + 1)?;
                        let text: String = chars[i + 1..end].iter().collect();
                        out.push('\\');
                        out.push_str(&text);
                        i = end;
                        quantifiable = true;
                        continue;
                    }
                    'x' => {
                        out.push_str(&format!("\\x{{{:X}}}", regex_hex_at(&chars, i + 2, 2)?));
                        i += 4;
                        quantifiable = true;
                        continue;
                    }
                    'u' => {
                        let code = regex_hex_at(&chars, i + 2, 4)?;
                        if (0xD800..=0xDFFF).contains(&code) {
                            return None;
                        }
                        out.push_str(&format!("\\x{{{code:X}}}"));
                        i += 6;
                        quantifiable = true;
                        continue;
                    }
                    _ if REGEX_SYNTAX_ESCAPES.contains(e) => {
                        out.push_str(&regex::escape(&e.to_string()))
                    }
                    _ if regex_control_escape(e).is_some() => {
                        out.push('\\');
                        out.push(e);
                    }
                    _ => return None,
                }
                i += 2;
                quantifiable = !matches!(e, 'b' | 'B');
            }
            _ => {
                out.push_str(&regex::escape(&c.to_string()));
                i += 1;
                quantifiable = true;
            }
        }
    }
    (depth == 0).then_some(out)
}

/// Replaces every match the way ECMAScript's global `replace` does: after a non-empty match an empty match at
/// its end still counts (the `regex` crate's own iteration skips it), and after an empty match the search
/// resumes one code point later. The replacement is literal.
fn replace_each_match(regex: &regex::Regex, text: &str, replacement: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut at = 0;
    while at <= text.len() {
        let Some(found) = regex.find_at(text, at) else {
            break;
        };
        out.push_str(&text[copied..found.start()]);
        out.push_str(replacement);
        copied = found.end();
        at = if found.is_empty() {
            found.end() + text[found.end()..].chars().next().map_or(1, char::len_utf8)
        } else {
            found.end()
        };
    }
    out.push_str(&text[copied..]);
    out
}

/// Compiles a pattern of the shared dialect; `None` when it is outside the dialect.
pub fn compile_shared_regex(pattern: &str) -> Option<regex::Regex> {
    regex::Regex::new(&translate_shared_regex(pattern)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_int(text: &str) -> Value {
        let expr: SExpression = serde_json::from_value(serde_json::json!(["str/to-int", text])).expect("s-expression");
        eval_expr(&expr, &BindingContext::default()).expect("evaluates")
    }

    #[test]
    fn to_int_reads_signed_ascii_decimal_integers() {
        assert_eq!(to_int("0"), Value::Number(0.0));
        assert_eq!(to_int("42"), Value::Number(42.0));
        assert_eq!(to_int("-42"), Value::Number(-42.0));
        assert_eq!(to_int("+5"), Value::Number(5.0));
        assert_eq!(to_int("007"), Value::Number(7.0));
        assert_eq!(to_int("-0"), Value::Number(0.0));
        assert_eq!(to_int("9007199254740991"), Value::Number(9_007_199_254_740_991.0));
        assert_eq!(to_int("-9007199254740991"), Value::Number(-9_007_199_254_740_991.0));
    }

    #[test]
    fn to_int_control_answers_null_for_anything_else() {
        for text in ["", "-", "+", " 5", "5 ", "1e3", "1.0", "0x10", "--1", "+-1", "١٢", "9007199254740992", "-9007199254740992", "99999999999999999999999"] {
            assert_eq!(to_int(text), Value::Null, "{text:?}");
        }
        let expr: SExpression = serde_json::from_value(serde_json::json!(["str/to-int", 5])).unwrap();
        assert!(eval_expr(&expr, &BindingContext::default()).is_err());
    }
}
