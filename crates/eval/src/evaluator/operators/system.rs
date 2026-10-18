//! Time, validation, format, lambda, and composition operators (the
//! system-facing remainder of the operator set).

use super::macros::*;
use super::util::deep_equals;
use super::*;

// ============================================================================
// Phase 2.5: Time Operations
// ============================================================================

/// time/today - Today at midnight UTC
pub struct TimeTodayOp;
impl Operator for TimeTodayOp {
    fn evaluate(&self, _args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        let now = ctx.now_millis() as i64;

        // Round down to midnight UTC
        let ms_per_day = 24 * 60 * 60 * 1000_i64;
        let today = (now / ms_per_day) * ms_per_day;

        Ok(Value::Number(today as f64))
    }
}

/// The one time-unit vocabulary. Every `time/*` operator resolves its unit
/// argument through `TimeUnit::parse`, and the JS interpreter's
/// `packages/almadar-evaluator/std/time.ts` mirrors this table exactly — the two
/// execution paths diverged silently once already
/// (`S-CALENDAR-RANGE-FILTER-USES-UNSUPPORTED-TIME-UNITS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeUnit {
    Millisecond,
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

/// Every spelling this vocabulary accepts, in one place so the error message
/// and the parser can never drift apart.
pub const TIME_UNIT_VOCABULARY: &str =
    "ms|millisecond(s), s|second(s), m|minute(s), h|hour(s), d|day(s), w|week(s), month(s), quarter(s), year(s)";

impl TimeUnit {
    pub fn parse(unit: &str) -> Result<Self, OrbitalError> {
        match unit {
            "ms" | "millisecond" | "milliseconds" => Ok(TimeUnit::Millisecond),
            "s" | "second" | "seconds" => Ok(TimeUnit::Second),
            "m" | "minute" | "minutes" => Ok(TimeUnit::Minute),
            "h" | "hour" | "hours" => Ok(TimeUnit::Hour),
            "d" | "day" | "days" => Ok(TimeUnit::Day),
            "w" | "week" | "weeks" => Ok(TimeUnit::Week),
            "month" | "months" => Ok(TimeUnit::Month),
            "quarter" | "quarters" => Ok(TimeUnit::Quarter),
            "year" | "years" => Ok(TimeUnit::Year),
            _ => Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: format!("time unit ({TIME_UNIT_VOCABULARY})"),
                actual: unit.to_string(),
            })),
        }
    }

    /// Fixed-length units expressed in milliseconds. `Month`/`Quarter`/`Year` have no
    /// fixed length and return `None` — callers must use calendar arithmetic.
    pub fn fixed_millis(self) -> Option<f64> {
        Some(match self {
            TimeUnit::Millisecond => 1.0,
            TimeUnit::Second => 1000.0,
            TimeUnit::Minute => 60.0 * 1000.0,
            TimeUnit::Hour => 60.0 * 60.0 * 1000.0,
            TimeUnit::Day => 24.0 * 60.0 * 60.0 * 1000.0,
            TimeUnit::Week => 7.0 * 24.0 * 60.0 * 60.0 * 1000.0,
            TimeUnit::Month | TimeUnit::Quarter | TimeUnit::Year => return None,
        })
    }
}

fn days_in_month(year: i32, month: i32) -> i32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 30,
    }
}

/// Calendar-correct month shift. The day-of-month **clamps** into the target
/// month (31 Mar − 1 month = 28/29 Feb) rather than overflowing into the next
/// one; time-of-day is preserved. The JS path clamps identically.
fn add_calendar_months(ts: f64, months: i64) -> f64 {
    let (y, m, d) = timestamp_to_date(ts);
    let time_of_day = ts - date_to_timestamp(y, m, d);
    let total = (y as i64) * 12 + (m as i64 - 1) + months;
    let ny = total.div_euclid(12) as i32;
    let nm = total.rem_euclid(12) as i32 + 1;
    date_to_timestamp(ny, nm, d.min(days_in_month(ny, nm))) + time_of_day
}

/// Signed count of **whole** calendar months from `from` to `to`. A partial
/// trailing month does not count, so 31 Jan → 28 Feb is 0 months, matching the
/// clamping in `add_calendar_months`.
fn whole_months_between(from: f64, to: f64) -> i64 {
    let (lo, hi) = (from.min(to), from.max(to));
    let (y1, m1, d1) = timestamp_to_date(lo);
    let (y2, m2, d2) = timestamp_to_date(hi);
    let tod1 = lo - date_to_timestamp(y1, m1, d1);
    let tod2 = hi - date_to_timestamp(y2, m2, d2);

    let mut months = ((y2 - y1) as i64) * 12 + (m2 - m1) as i64;
    if (d2, tod2) < (d1, tod1) {
        months -= 1;
    }
    if to < from {
        -months
    } else {
        months
    }
}

/// Shift `ts` by `amount` of `unit`. The single implementation behind both
/// `time/add` and `time/subtract`.
fn shift_time(ts: f64, amount: f64, unit: TimeUnit) -> f64 {
    match unit.fixed_millis() {
        Some(ms) => ts + amount * ms,
        None => {
            let months = match unit {
                TimeUnit::Year => amount as i64 * 12,
                TimeUnit::Quarter => amount as i64 * 3,
                _ => amount as i64,
            };
            add_calendar_months(ts, months)
        }
    }
}

/// time/add - Add duration to timestamp
pub struct TimeAddOp;
impl Operator for TimeAddOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        // ["time/add", timestamp, amount, unit]
        require_arity!(args, "time/add", >= 3);

        let ts = require_number!(eval_expr(&args[0], ctx)?);
        let amount = require_number!(eval_expr(&args[1], ctx)?);
        let unit = TimeUnit::parse(&eval_expr(&args[2], ctx)?.to_string_value())?;

        Ok(Value::Number(shift_time(ts, amount, unit)))
    }
}

/// time/subtract - Subtract duration from timestamp
pub struct TimeSubtractOp;
impl Operator for TimeSubtractOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        // ["time/subtract", timestamp, amount, unit]
        require_arity!(args, "time/subtract", >= 3);

        let ts = require_number!(eval_expr(&args[0], ctx)?);
        let amount = require_number!(eval_expr(&args[1], ctx)?);
        let unit = TimeUnit::parse(&eval_expr(&args[2], ctx)?.to_string_value())?;

        Ok(Value::Number(shift_time(ts, -amount, unit)))
    }
}

/// time/diff - Difference between timestamps
pub struct TimeDiffOp;
impl Operator for TimeDiffOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        // ["time/diff", ts1, ts2, unit?]
        require_arity!(args, "time/diff", >= 2);

        let ts1 = require_number!(eval_expr(&args[0], ctx)?);

        let ts2 = require_number!(eval_expr(&args[1], ctx)?);

        // Signed, so that diff(a, b) == -diff(b, a). Whole units truncate
        // toward zero for the same reason.
        let diff_ms = ts1 - ts2;

        if args.len() > 2 {
            let unit = TimeUnit::parse(&eval_expr(&args[2], ctx)?.to_string_value())?;
            let result = match unit.fixed_millis() {
                Some(ms) => (diff_ms / ms).trunc(),
                None => {
                    let months = whole_months_between(ts2, ts1) as f64;
                    match unit {
                        TimeUnit::Year => (months / 12.0).trunc(),
                        TimeUnit::Quarter => (months / 3.0).trunc(),
                        _ => months,
                    }
                }
            };
            Ok(Value::Number(result))
        } else {
            // Default to milliseconds
            Ok(Value::Number(diff_ms))
        }
    }
}

/// time/isBefore - Compare timestamps
pub struct TimeIsBeforeOp;
impl Operator for TimeIsBeforeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/isBefore", >= 2);

        let ts1 = require_number!(eval_expr(&args[0], ctx)?);

        let ts2 = require_number!(eval_expr(&args[1], ctx)?);

        Ok(Value::Bool(ts1 < ts2))
    }
}

/// time/isAfter - Compare timestamps
pub struct TimeIsAfterOp;
impl Operator for TimeIsAfterOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/isAfter", >= 2);

        let ts1 = require_number!(eval_expr(&args[0], ctx)?);

        let ts2 = require_number!(eval_expr(&args[1], ctx)?);

        Ok(Value::Bool(ts1 > ts2))
    }
}

/// `YYYY-MM-DD[THH:MM[:SS[.f+]][Z|±HH:MM]]` to epoch ms; a date is midnight UTC and a date-time
/// without an offset is read as UTC. `None` for every other spelling or an out-of-range part.
fn parse_iso_instant(text: &str) -> Option<i64> {
    use chrono::{NaiveDate, TimeZone};
    let b = text.as_bytes();
    let num = |from: usize, len: usize| -> Option<u32> {
        let slice = b.get(from..from + len)?;
        if slice.iter().all(u8::is_ascii_digit) {
            std::str::from_utf8(slice).ok()?.parse().ok()
        } else {
            None
        }
    };
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') {
        return None;
    }
    let date = NaiveDate::from_ymd_opt(num(0, 4)? as i32, num(5, 2)?, num(8, 2)?)?;
    let mut ms = chrono::Utc
        .from_utc_datetime(&date.and_hms_opt(0, 0, 0)?)
        .timestamp_millis();
    if b.len() == 10 {
        return Some(ms);
    }
    if b.get(10) != Some(&b'T') || b.get(13) != Some(&b':') {
        return None;
    }
    let (hour, minute) = (num(11, 2)?, num(14, 2)?);
    if hour > 23 || minute > 59 {
        return None;
    }
    ms += i64::from(hour) * 3_600_000 + i64::from(minute) * 60_000;
    let mut at = 16;
    if b.get(at) == Some(&b':') {
        let second = num(at + 1, 2)?;
        if second > 59 {
            return None;
        }
        ms += i64::from(second) * 1000;
        at += 3;
        if b.get(at) == Some(&b'.') {
            let start = at + 1;
            let mut end = start;
            while b.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            if end == start {
                return None;
            }
            let padded = format!("{:0<3}", &text[start..end]);
            ms += padded[..3].parse::<i64>().ok()?;
            at = end;
        }
    }
    match &text[at..] {
        "" | "Z" => Some(ms),
        zone if zone.len() == 6 && matches!(b[at], b'+' | b'-') && b[at + 3] == b':' => {
            let (oh, om) = (num(at + 1, 2)?, num(at + 4, 2)?);
            if oh > 23 || om > 59 {
                return None;
            }
            let offset = (i64::from(oh) * 60 + i64::from(om)) * 60_000;
            Some(if b[at] == b'-' {
                ms + offset
            } else {
                ms - offset
            })
        }
        _ => None,
    }
}

/// time/parse - RFC 3339 / ISO 8601 date or date-time to epoch ms; anything else is an error.
pub struct TimeParseOp;
impl Operator for TimeParseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/parse", 1);

        let date_str = match eval_expr(&args[0], ctx)? {
            Value::String(s) => s,
            other => {
                return Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                    "time/parse: expected a string argument, got {}",
                    other.type_name()
                ))))
            }
        };
        match parse_iso_instant(&date_str) {
            Some(ms) => Ok(Value::Number(ms as f64)),
            None => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "time/parse: \"{date_str}\" is not an ISO 8601 date or date-time"
            )))),
        }
    }
}

/// time/valid - true iff time/parse would succeed; null is false
pub struct TimeValidOp;
impl Operator for TimeValidOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/valid", 1);
        match eval_expr(&args[0], ctx)? {
            Value::Null => Ok(Value::Bool(false)),
            Value::String(s) => Ok(Value::Bool(parse_iso_instant(&s).is_some())),
            other => Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "time/valid: expected a string argument, got {}",
                other.type_name()
            )))),
        }
    }
}

/// time/format's 2-arg (timestamp, token-string) form: moment/dayjs-style
/// token replacement (`"MMM D, YYYY"` → `"Sep 18, 2026"`). `require_arity!`
/// only checks "at least 1 arg", so a 2-arg call silently fell through to
/// the 1-arg ISO branch below with the format string entirely ignored —
/// this Evaluator is the ONE canonical definition of every operator's
/// semantics (orbital-core/src/evaluator), and it simply never implemented
/// this call shape (C-TIME-FORMAT-TOKEN-ARG-DROPPED). `orbital-shell-
/// typescript`'s codegen (backend/../codegen/operator.rs,
/// TIME_FORMAT_TOKEN_TEMPLATE) emits an equivalent single-pass, longest-
/// token-first substitution for the compiled path — same token
/// vocabulary, kept in sync by hand since generated JS obviously can't call
/// back into this Rust function.
fn format_timestamp_tokens(ts_ms: f64, format: &str) -> String {
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ts_ms as i64)
        .unwrap_or_else(|| chrono::DateTime::<chrono::Utc>::from_timestamp_millis(0).unwrap());
    use chrono::{Datelike, Timelike};

    let weekday_short = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let weekday_long = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    let month_short = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month_long = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    // `.num_days_from_sunday()` matches JS `Date.getDay()` (0 = Sunday).
    let dow = dt.weekday().num_days_from_sunday() as usize;
    let month_idx = (dt.month() - 1) as usize;

    // Longest-first so a scan at any position never matches a short token
    // (`M`/`D`) inside a longer one (`MMMM`/`DD`) — same rule the JS side
    // enforces by sorting its regex alternation the same way.
    let tokens: &[(&str, String)] = &[
        ("YYYY", format!("{:04}", dt.year())),
        ("MMMM", month_long[month_idx].to_string()),
        ("dddd", weekday_long[dow].to_string()),
        ("MMM", month_short[month_idx].to_string()),
        ("ddd", weekday_short[dow].to_string()),
        ("YY", format!("{:02}", dt.year().rem_euclid(100))),
        ("MM", format!("{:02}", dt.month())),
        ("DD", format!("{:02}", dt.day())),
        ("HH", format!("{:02}", dt.hour())),
        ("mm", format!("{:02}", dt.minute())),
        ("ss", format!("{:02}", dt.second())),
        ("M", dt.month().to_string()),
        ("D", dt.day().to_string()),
        ("H", dt.hour().to_string()),
        ("m", dt.minute().to_string()),
        ("s", dt.second().to_string()),
    ];

    let bytes = format.as_bytes();
    let mut out = String::with_capacity(format.len());
    let mut i = 0;
    'outer: while i < bytes.len() {
        for (token, value) in tokens {
            if format[i..].starts_with(token) {
                out.push_str(value);
                i += token.len();
                continue 'outer;
            }
        }
        // `format` is ASCII token syntax; char_indices avoids splitting a
        // multi-byte char if the literal text around the tokens isn't.
        let ch = format[i..].chars().next().expect("i < bytes.len()");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// time/format - Format timestamp to string (basic support)
pub struct TimeFormatOp;
impl Operator for TimeFormatOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/format", 1);

        let ts = require_number!(eval_expr(&args[0], ctx)?);

        if let Some(format_expr) = args.get(1) {
            let format = eval_expr(format_expr, ctx)?.to_string_value();
            return Ok(Value::String(format_timestamp_tokens(ts, &format)));
        }

        // Basic formatting - convert to ISO date string
        let total_days = (ts / (24.0 * 60.0 * 60.0 * 1000.0)) as i64;
        let remaining_ms = (ts % (24.0 * 60.0 * 60.0 * 1000.0)) as i64;

        // Calculate year (approximate)
        let mut year = 1970 + (total_days / 365) as i32;
        let mut day_of_year = (total_days % 365) as i32;

        // Adjust for accumulated leap years
        let leap_years = ((year - 1969) / 4) - ((year - 1901) / 100) + ((year - 1601) / 400);
        day_of_year -= leap_years;
        while day_of_year < 0 {
            year -= 1;
            day_of_year += if is_leap_year(year) { 366 } else { 365 };
        }

        // Calculate month and day
        let month_days = if is_leap_year(year) {
            [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
        } else {
            [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
        };

        let mut month = 1;
        let mut day = day_of_year + 1;
        for days in month_days.iter() {
            if day <= *days {
                break;
            }
            day -= days;
            month += 1;
        }

        let hours = (remaining_ms / (60 * 60 * 1000)) % 24;
        let minutes = (remaining_ms / (60 * 1000)) % 60;
        let seconds = (remaining_ms / 1000) % 60;

        Ok(Value::String(format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            year,
            month,
            day.max(1),
            hours,
            minutes,
            seconds
        )))
    }
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

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
        Ok(Value::Bool(matches!(val, Value::String(_))))
    }
}

/// validate/number - Type check
pub struct ValidateNumberOp;
impl Operator for ValidateNumberOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/number", 1);
        let val = eval_expr(&args[0], ctx)?;
        Ok(Value::Bool(matches!(val, Value::Number(_))))
    }
}

/// validate/boolean - Type check
pub struct ValidateBooleanOp;
impl Operator for ValidateBooleanOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/boolean", 1);
        let val = eval_expr(&args[0], ctx)?;
        Ok(Value::Bool(matches!(val, Value::Bool(_))))
    }
}

/// validate/array - Type check
pub struct ValidateArrayOp;
impl Operator for ValidateArrayOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/array", 1);
        let val = eval_expr(&args[0], ctx)?;
        Ok(Value::Bool(matches!(val, Value::Array(_))))
    }
}

/// validate/object - Type check
pub struct ValidateObjectOp;
impl Operator for ValidateObjectOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "validate/object", 1);
        let val = eval_expr(&args[0], ctx)?;
        Ok(Value::Bool(matches!(val, Value::Object(_))))
    }
}

/// Whether `value` matches the pattern the installed operator table declares for the semantic validator `operator`
/// (`validate/email`, `validate/url`, `validate/phone`, `validate/uuid`). The pattern is `@almadar/core`
/// `SEMANTIC_STRING_PATTERNS`, carried by the table; a table that lacks it is an error, never a fallback.
pub fn matches_semantic_pattern(operator: &str, value: &str) -> Result<bool, OrbitalError> {
    static COMPILED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, regex::Regex>>> = std::sync::OnceLock::new();
    let cache = COMPILED.get_or_init(Default::default);
    let mut cache = cache.lock().map_err(|_| {
        OrbitalError::Eval(EvalError::InvalidArgument(format!("{operator}: pattern cache poisoned")))
    })?;
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
        Ok(Value::Bool(matches_semantic_pattern("validate/email", &val)?))
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
// Phase 3.5: Time Advanced
// ============================================================================

/// time/year - Extract year from timestamp
pub struct TimeYearOp;
impl Operator for TimeYearOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/year", 1);

        let ts = require_number!(eval_expr(&args[0], ctx)?);

        let total_days = (ts / (24.0 * 60.0 * 60.0 * 1000.0)) as i64;
        let year = estimate_year_from_days(total_days);

        Ok(Value::Number(year as f64))
    }
}

/// time/month - Extract month from timestamp (1-12)
pub struct TimeMonthOp;
impl Operator for TimeMonthOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/month", 1);

        let ts = require_number!(eval_expr(&args[0], ctx)?);

        let (_, month, _) = timestamp_to_date(ts);
        Ok(Value::Number(month as f64))
    }
}

/// time/day - Extract day from timestamp (1-31)
pub struct TimeDayOp;
impl Operator for TimeDayOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/day", 1);

        let ts = require_number!(eval_expr(&args[0], ctx)?);

        let (_, _, day) = timestamp_to_date(ts);
        Ok(Value::Number(day as f64))
    }
}

/// time/weekday - Day of week (0-6, Sunday=0)
pub struct TimeWeekdayOp;
impl Operator for TimeWeekdayOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/weekday", 1);

        let ts = require_number!(eval_expr(&args[0], ctx)?);

        // Days since epoch (Jan 1, 1970 was Thursday = 4)
        let total_days = (ts / (24.0 * 60.0 * 60.0 * 1000.0)) as i64;
        let weekday = ((total_days % 7) + 4) % 7; // Adjust for Thursday start

        Ok(Value::Number(weekday as f64))
    }
}

/// time/hour - Extract hour (0-23)
pub struct TimeHourOp;
impl Operator for TimeHourOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/hour", 1);

        let ts = require_number!(eval_expr(&args[0], ctx)?);

        let ms_per_day = 24.0 * 60.0 * 60.0 * 1000.0;
        let remaining_ms = ts % ms_per_day;
        let hour = (remaining_ms / (60.0 * 60.0 * 1000.0)) as i64 % 24;

        Ok(Value::Number(hour as f64))
    }
}

/// time/minute - Extract minute (0-59)
pub struct TimeMinuteOp;
impl Operator for TimeMinuteOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/minute", 1);

        let ts = require_number!(eval_expr(&args[0], ctx)?);

        let minute = ((ts / (60.0 * 1000.0)) as i64) % 60;

        Ok(Value::Number(minute as f64))
    }
}

/// time/second - Extract second (0-59)
pub struct TimeSecondOp;
impl Operator for TimeSecondOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/second", 1);

        let ts = require_number!(eval_expr(&args[0], ctx)?);

        let second = ((ts / 1000.0) as i64) % 60;

        Ok(Value::Number(second as f64))
    }
}

/// time/isBetween - Check if timestamp is between two others
pub struct TimeIsBetweenOp;
impl Operator for TimeIsBetweenOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/isBetween", >= 3);

        let ts = require_number!(eval_expr(&args[0], ctx)?);
        let start = require_number!(eval_expr(&args[1], ctx)?);
        let end = require_number!(eval_expr(&args[2], ctx)?);

        Ok(Value::Bool(ts >= start && ts <= end))
    }
}

/// time/isSame - Check if timestamps are the same (optionally by unit)
pub struct TimeIsSameOp;
impl Operator for TimeIsSameOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/isSame", >= 2);

        let ts1 = require_number!(eval_expr(&args[0], ctx)?);
        let ts2 = require_number!(eval_expr(&args[1], ctx)?);

        if args.len() > 2 {
            // Same unit-bucket, i.e. startOf(a, unit) == startOf(b, unit).
            // Defined in terms of `start_of` so week/month/year cannot drift
            // back to an exact-millisecond comparison.
            let unit = TimeUnit::parse(&eval_expr(&args[2], ctx)?.to_string_value())?;
            Ok(Value::Bool(start_of(ts1, unit) == start_of(ts2, unit)))
        } else {
            // Exact match
            Ok(Value::Bool((ts1 - ts2).abs() < 1.0))
        }
    }
}

// Helper functions for time operations
fn estimate_year_from_days(total_days: i64) -> i32 {
    // Approximate year from days since epoch
    let mut year = 1970 + (total_days / 365) as i32;
    let mut days_remaining = total_days % 365;

    // Adjust for leap years
    let leap_years = count_leap_years(1970, year);
    days_remaining -= leap_years as i64;

    while days_remaining < 0 {
        year -= 1;
        days_remaining += if is_leap_year(year) { 366 } else { 365 };
    }

    year
}

fn count_leap_years(from: i32, to: i32) -> i32 {
    let count_leaps = |y: i32| -> i32 { (y / 4) - (y / 100) + (y / 400) };
    count_leaps(to - 1) - count_leaps(from - 1)
}

fn timestamp_to_date(ts: f64) -> (i32, i32, i32) {
    let total_days = (ts / (24.0 * 60.0 * 60.0 * 1000.0)) as i64;
    let year = estimate_year_from_days(total_days);

    // Calculate day of year
    let year_start_days = days_since_epoch(year);
    let day_of_year = (total_days - year_start_days) as i32;

    let month_days = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 1;
    let mut day = day_of_year + 1;
    for days in month_days.iter() {
        if day <= *days {
            break;
        }
        day -= days;
        month += 1;
    }

    (year, month.min(12), day.clamp(1, 31))
}

fn days_since_epoch(year: i32) -> i64 {
    let years = year - 1970;
    let leap_years = count_leap_years(1970, year);
    (years as i64) * 365 + (leap_years as i64)
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
        Ok(Value::Bool(matches_semantic_pattern("validate/uuid", &val)?))
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
// Phase 4.5: Time Extras
// ============================================================================

/// time/isPast - Check if timestamp in past
pub struct TimeIsPastOp;
impl Operator for TimeIsPastOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/isPast", 1);
        let ts = require_number!(eval_expr(&args[0], ctx)?);

        let now = ctx.now_millis();

        Ok(Value::Bool(ts < now))
    }
}

/// time/isFuture - Check if timestamp in future
pub struct TimeIsFutureOp;
impl Operator for TimeIsFutureOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/isFuture", 1);
        let ts = require_number!(eval_expr(&args[0], ctx)?);

        let now = ctx.now_millis();

        Ok(Value::Bool(ts > now))
    }
}

/// time/isToday - Check if same day as today
pub struct TimeIsTodayOp;
impl Operator for TimeIsTodayOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/isToday", 1);
        let ts = require_number!(eval_expr(&args[0], ctx)?);

        let now = ctx.now_millis();

        // Convert to days since epoch
        const MS_PER_DAY: f64 = 86400000.0;
        let ts_day = (ts / MS_PER_DAY).floor();
        let now_day = (now / MS_PER_DAY).floor();

        Ok(Value::Bool((ts_day - now_day).abs() < f64::EPSILON))
    }
}

/// time/duration - Format duration human-readable
pub struct TimeDurationOp;
impl Operator for TimeDurationOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/duration", 1);
        let ms = require_number!(eval_expr(&args[0], ctx)?);

        let total_seconds = (ms / 1000.0).abs() as u64;
        let days = total_seconds / 86400;
        let hours = (total_seconds % 86400) / 3600;
        let minutes = (total_seconds % 3600) / 60;
        let seconds = total_seconds % 60;

        let mut parts = Vec::new();
        if days > 0 {
            parts.push(format!("{}d", days));
        }
        if hours > 0 {
            parts.push(format!("{}h", hours));
        }
        if minutes > 0 {
            parts.push(format!("{}m", minutes));
        }
        if seconds > 0 || parts.is_empty() {
            parts.push(format!("{}s", seconds));
        }

        Ok(Value::String(parts.join(" ")))
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
        Ok(Value::Bool(matches_semantic_pattern("validate/phone", &val)?))
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
type FieldRules = Vec<(String, Vec<Value>)>;

fn rule_list(field: &str, list: Vec<Value>) -> Result<FieldRules, OrbitalError> {
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
                    Some(Value::String(name)) => Ok((name, parts.collect())),
                    _ => Err(broken("a list without a rule name")),
                }
            }
            other => Err(broken(other.type_name())),
        })
        .collect()
}

/// The rules argument of `validate/check`: an object of field to rule list. A literal object is read as data, so
/// a rule `["minLength", 2]` is a rule and not a call of `validate/minLength`; its rule arguments are evaluated.
fn rules_by_field(expr: &SExpression, ctx: &BindingContext) -> Result<Vec<(String, FieldRules)>, OrbitalError> {
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
                        SExpression::Array(parts) if matches!(parts.first(), Some(SExpression::String(_))) => {
                            let mut out = vec![eval_literal_head(&parts[0])];
                            for part in &parts[1..] {
                                out.push(eval_expr(part, ctx)?);
                            }
                            Ok(Value::Array(out))
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
                Value::Array(rules) => Ok((field.clone(), rule_list(&field, rules)?)),
                other => Err(mismatch(other.type_name())),
            })
            .collect(),
        Value::Null => Ok(Vec::new()),
        other => Err(mismatch(other.type_name())),
    }
}

fn eval_literal_head(head: &SExpression) -> Value {
    match head {
        SExpression::String(name) => Value::String(name.clone()),
        _ => Value::Null,
    }
}

fn rule_holds(name: &str, value: &Value, args: &[Value]) -> Result<Option<bool>, OrbitalError> {
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
        "required" => !matches!(value, Value::Null) && !matches!(value, Value::String(text) if text.is_empty()),
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
        "range" => matches!(value, Value::Number(n) if number(0).zip(number(1)).is_some_and(|(lo, hi)| *n >= lo && *n <= hi)),
        "pattern" => match (value, args.first()) {
            (Value::String(text), Some(Value::String(pattern))) => {
                super::string::compile_shared_regex(pattern).is_some_and(|re| re.is_match(text))
            }
            _ => false,
        },
        "oneOf" => matches!(args.first(), Some(Value::Array(options)) if options.iter().any(|opt| opt.loose_eq(value))),
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
                    errors.push(Value::String(format!("{field}: {name} validation failed")));
                }
            }
        }
        let mut out = ValueMap::new();
        out.insert("valid".to_string(), Value::Bool(errors.is_empty()));
        out.insert("errors".to_string(), Value::Array(errors));
        Ok(Value::Object(out))
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

// ============================================================================
// Format operators - convert values to formatted strings
// ============================================================================

/// format/number - Format number with locale options
pub struct FormatNumberOp;
impl Operator for FormatNumberOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/number", 1);
        let val = eval_expr(&args[0], ctx)?;
        let num = val.to_number().unwrap_or(0.0);
        // Simple thousands separator formatting
        let formatted = format!("{:.0}", num)
            .chars()
            .rev()
            .collect::<Vec<_>>()
            .chunks(3)
            .map(|chunk| chunk.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join(",")
            .chars()
            .rev()
            .collect::<String>();
        Ok(Value::String(formatted))
    }
}

/// format/percent - Format as percentage
pub struct FormatPercentOp;
impl Operator for FormatPercentOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/percent", 1);
        let val = eval_expr(&args[0], ctx)?;
        let num = val.to_number().unwrap_or(0.0);
        Ok(Value::String(format!("{:.0}%", num * 100.0)))
    }
}

/// format/currency - Format as currency
pub struct FormatCurrencyOp;
impl Operator for FormatCurrencyOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/currency", 1);
        let val = eval_expr(&args[0], ctx)?;
        let num = val.to_number().unwrap_or(0.0);
        let symbol = if args.len() > 1 {
            eval_expr(&args[1], ctx)?.to_string_value()
        } else {
            "$".to_string()
        };
        Ok(Value::String(format!("{}{:.2}", symbol, num)))
    }
}

/// format/bytes - Format bytes to human-readable
pub struct FormatBytesOp;
impl Operator for FormatBytesOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/bytes", 1);
        let val = eval_expr(&args[0], ctx)?;
        let bytes = val.to_number().unwrap_or(0.0);
        let units = ["B", "KB", "MB", "GB", "TB"];
        let mut size = bytes;
        let mut unit_idx = 0;
        while size >= 1024.0 && unit_idx < units.len() - 1 {
            size /= 1024.0;
            unit_idx += 1;
        }
        Ok(Value::String(format!("{:.1} {}", size, units[unit_idx])))
    }
}

/// format/ordinal - Format number as ordinal (1st, 2nd, etc.)
pub struct FormatOrdinalOp;
impl Operator for FormatOrdinalOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/ordinal", 1);
        let val = eval_expr(&args[0], ctx)?;
        let num = val.to_number().unwrap_or(0.0) as i64;
        let suffix = match (num % 10, num % 100) {
            (1, 11) | (2, 12) | (3, 13) => "th",
            (1, _) => "st",
            (2, _) => "nd",
            (3, _) => "rd",
            _ => "th",
        };
        Ok(Value::String(format!("{}{}", num, suffix)))
    }
}

/// format/phone - Format phone number
pub struct FormatPhoneOp;
impl Operator for FormatPhoneOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/phone", 1);
        let val = eval_expr(&args[0], ctx)?;
        let digits: String = val
            .to_string_value()
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect();
        let formatted = if digits.len() == 10 {
            format!("({}) {}-{}", &digits[0..3], &digits[3..6], &digits[6..10])
        } else if digits.len() == 11 && digits.starts_with('1') {
            format!(
                "+1 ({}) {}-{}",
                &digits[1..4],
                &digits[4..7],
                &digits[7..11]
            )
        } else {
            digits
        };
        Ok(Value::String(formatted))
    }
}

/// format/plural - Format with plural form
pub struct FormatPluralOp;
impl Operator for FormatPluralOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/plural", >= 3);
        let count = eval_expr(&args[0], ctx)?.to_number().unwrap_or(0.0) as i64;
        let singular = eval_expr(&args[1], ctx)?.to_string_value();
        let plural = eval_expr(&args[2], ctx)?.to_string_value();
        let word = if count == 1 { singular } else { plural };
        Ok(Value::String(format!("{} {}", count, word)))
    }
}

/// format/list - Format array as list
pub struct FormatListOp;
impl Operator for FormatListOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/list", 1);
        let val = eval_expr(&args[0], ctx)?;
        let array = match val {
            Value::Array(a) => a,
            _ => return Ok(Value::String(val.to_string_value())),
        };
        let items: Vec<String> = array.iter().map(|v| v.to_string_value()).collect();
        let formatted = match items.len() {
            0 => String::new(),
            1 => items[0].clone(),
            2 => format!("{} and {}", items[0], items[1]),
            _ => {
                let last = items.last().unwrap();
                let rest = &items[..items.len() - 1];
                format!("{}, and {}", rest.join(", "), last)
            }
        };
        Ok(Value::String(formatted))
    }
}

// ============================================================================
// Lambda Operator (Phase 1)
// ============================================================================

pub struct FnOp;
impl Operator for FnOp {
    fn evaluate(&self, args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        // ["fn", "x", body] or ["fn", ["a", "b"], body]
        require_arity!(args, "fn", >= 2);

        // Parse parameters
        let params = match &args[0] {
            SExpression::String(name) => vec![name.clone()],
            SExpression::Array(arr) => arr
                .iter()
                .filter_map(|p| {
                    if let SExpression::String(s) = p {
                        Some(s.clone())
                    } else {
                        None
                    }
                })
                .collect(),
            _ => {
                return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "string or array of strings".to_string(),
                    actual: "invalid parameter list".to_string(),
                }))
            }
        };

        // Store body as-is (not evaluated yet)
        let body = std::sync::Arc::new(args[1].clone());
        super::super::trace::adopt(&body, &args[1]);

        Ok(Value::Lambda { params, body })
    }
}

/// `(quote x)` — an S-expression held as data (G-CROSS-041): the encoded body
/// (`schema::quote`) decodes to `x`, returned without evaluating or resolving it.
pub struct QuoteOp;
impl Operator for QuoteOp {
    fn evaluate(&self, args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "quote", 1);
        let SExpression::String(body) = &args[0] else {
            return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "an encoded quote body (string)".to_string(),
                actual: "a non-string quote argument".to_string(),
            }));
        };
        orbital_core_base::schema::quote::decode_quote_body(body)
            .map(Value::from)
            .ok_or_else(|| {
                OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "an encoded quote body (JSON)".to_string(),
                    actual: "a malformed quote body".to_string(),
                })
            })
    }
}

/// `(quasiquote x)` — a quoted template whose holes are live: the body
/// (`schema::quote::quasiquote_expr`) decodes to the template, each hole
/// expression after it is evaluated, and marker `["unquote", i]` becomes
/// hole `i`'s value. Twin of `@almadar/core` `instantiateQuasiquote`.
pub struct QuasiquoteOp;

fn unquote_marker(value: &Value) -> Option<usize> {
    match value {
        Value::Array(items) if items.len() == 2 => match (&items[0], &items[1]) {
            (Value::String(h), Value::Number(n))
                if h == orbital_core_base::schema::quote::UNQUOTE_HEAD
                    && *n >= 0.0
                    && n.fract() == 0.0 =>
            {
                Some(*n as usize)
            }
            _ => None,
        },
        _ => None,
    }
}

/// The kind of the first value inside `value` a program cannot hold as data
/// (a lambda), matching `@almadar/evaluator`'s `toProgramData` refusal.
fn non_data_kind(value: &Value) -> Option<&'static str> {
    match value {
        Value::Array(items) => items.iter().find_map(non_data_kind),
        Value::Object(map) => map.values().find_map(non_data_kind),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
        Value::Lambda { .. } => Some("function"),
    }
}

fn count_markers(value: &Value) -> usize {
    if unquote_marker(value).is_some() {
        return 1;
    }
    match value {
        Value::Array(items) => items.iter().map(count_markers).sum(),
        Value::Object(map) => map.values().map(count_markers).sum(),
        _ => 0,
    }
}

fn fill_markers(value: Value, holes: &[Value]) -> Value {
    if let Some(i) = unquote_marker(&value) {
        return holes[i].clone();
    }
    match value {
        Value::Array(items) => {
            Value::Array(items.into_iter().map(|v| fill_markers(v, holes)).collect())
        }
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, fill_markers(v, holes)))
                .collect(),
        ),
        other => other,
    }
}

impl Operator for QuasiquoteOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        let Some(SExpression::String(body)) = args.first() else {
            return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "an encoded quasiquote body (string)".to_string(),
                actual: "a non-string quasiquote argument".to_string(),
            }));
        };
        let template = orbital_core_base::schema::quote::decode_quote_body(body)
            .map(Value::from)
            .ok_or_else(|| {
                OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "an encoded quasiquote body (JSON)".to_string(),
                    actual: "a malformed quasiquote body".to_string(),
                })
            })?;
        let holes = args[1..]
            .iter()
            .map(|e| super::eval_expr(e, ctx))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(kind) = holes.iter().find_map(non_data_kind) {
            return Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "quasiquote: a hole evaluated to a {kind} value, which cannot be held as program data"
            ))));
        }
        let markers = count_markers(&template);
        if markers != holes.len() {
            return Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                "quasiquote: template has {markers} hole(s), got {} value(s)",
                holes.len()
            ))));
        }
        Ok(fill_markers(template, &holes))
    }
}

/// `(unquote e)` evaluated on its own: legal only as a hole inside
/// `(quasiquote …)`, which replaces it before evaluation.
pub struct UnquoteOp;
impl Operator for UnquoteOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        Err(OrbitalError::Eval(EvalError::InvalidArgument(
            "unquote: legal only inside a quasiquote".to_string(),
        )))
    }
}

/// `(behavior/apply value overrides)` — records call-site overrides on a trait
/// value (`schema::behavior_value`). Twin of `@almadar/core` `applyTraitOverrides`.
pub struct BehaviorApplyOp;
impl Operator for BehaviorApplyOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "behavior/apply", >= 2);
        if args.len() > 2 {
            return Err(OrbitalError::Eval(EvalError::ArityMismatch {
                operator: "behavior/apply".to_string(),
                expected: 2,
                actual: args.len(),
            }));
        }
        let value = super::eval_expr(&args[0], ctx)?;
        let overrides = super::eval_expr(&args[1], ctx)?;
        orbital_core_base::schema::behavior_value::apply_behavior_overrides(&value, &overrides)
            .ok_or_else(|| {
                OrbitalError::Eval(EvalError::TypeMismatch {
                    expected: "a behavior value and overrides of its kind".to_string(),
                    actual: "a non-behavior value, or overrides with a key its kind lacks"
                        .to_string(),
                })
            })
    }
}

/// `(behavior/ref specifier)` — a whole behavior as a value. Twin of
/// `@almadar/evaluator` `evalBehaviorRef`.
pub struct BehaviorRefOp;
impl Operator for BehaviorRefOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "behavior/ref", >= 1);
        if args.len() > 1 {
            return Err(OrbitalError::Eval(EvalError::ArityMismatch {
                operator: "behavior/ref".to_string(),
                expected: 1,
                actual: args.len(),
            }));
        }
        let spec = super::eval_expr(&args[0], ctx)?;
        let Value::String(spec) = spec else {
            return Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "a behavior specifier string".to_string(),
                actual: format!("{spec:?}"),
            }));
        };
        orbital_core_base::schema::behavior_value::behavior_ref(&spec).ok_or_else(|| {
            OrbitalError::Eval(EvalError::TypeMismatch {
                expected:
                    "a behavior specifier (<prefix>/<name>, std/behaviors/<name> or ./<dir>/<name>)"
                        .to_string(),
                actual: spec,
            })
        })
    }
}

// ============================================================================
// Time Operators (Phase 1)
// ============================================================================

pub struct TimeNowOp;
impl Operator for TimeNowOp {
    fn evaluate(&self, _args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        // Return current Unix timestamp in milliseconds
        let now = ctx.now_millis();
        Ok(Value::Number(now))
    }
}

// ============================================================================
// Composition (Behavior) - parity placeholder
// ============================================================================
//
// `behavior/compose`, `behavior/wire`, `behavior/detect-layout`, and
// `behavior/pipe` are compile-time composition operators. They are resolved
// natively by the orbital-compiler `Compose` phase (Phase 0 sub-unit 0.3) and
// are never expected to reach the runtime evaluator. The stub below exists
// only to keep `operators.json` ↔ `operators.rs` parity green; if it is ever
// invoked at runtime, it raises a clear error so the compiler bug is obvious.
pub struct BehaviorCompositionStubOp(pub &'static str);
impl Operator for BehaviorCompositionStubOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
            "'{}' is a compile-time composition operator and must be resolved by the orbital-compiler Compose phase, not evaluated at runtime",
            self.0
        ))))
    }
}

/// Unix-ms for a Y/M/D at 00:00:00 UTC, using this file's no-chrono date math.
fn date_to_timestamp(year: i32, month: i32, day: i32) -> f64 {
    let month_days: [i64; 12] = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut days = days_since_epoch(year);
    for days_in_month in month_days
        .iter()
        .take(((month as usize).saturating_sub(1)).min(11))
    {
        days += days_in_month;
    }
    days += (day - 1) as i64;
    (days as f64) * 24.0 * 60.0 * 60.0 * 1000.0
}

/// Start of the unit-bucket containing `ts`. The single implementation behind
/// `time/startOf` and `time/isSame`. Weeks start **Monday** (ISO-8601).
fn start_of(ts: f64, unit: TimeUnit) -> f64 {
    let day_ms = 24.0 * 60.0 * 60.0 * 1000.0;
    match unit {
        TimeUnit::Millisecond => ts.floor(),
        TimeUnit::Second => (ts / 1000.0).floor() * 1000.0,
        TimeUnit::Minute => (ts / 60_000.0).floor() * 60_000.0,
        TimeUnit::Hour => (ts / 3_600_000.0).floor() * 3_600_000.0,
        TimeUnit::Day => (ts / day_ms).floor() * day_ms,
        TimeUnit::Week => {
            let days = (ts / day_ms).floor() as i64;
            let dow = (days + 3).rem_euclid(7); // Jan 1 1970 = Thu; 0 => Monday
            ((days - dow) as f64) * day_ms
        }
        TimeUnit::Month => {
            let (y, m, _) = timestamp_to_date(ts);
            date_to_timestamp(y, m, 1)
        }
        TimeUnit::Quarter => {
            let (y, m, _) = timestamp_to_date(ts);
            date_to_timestamp(y, (m - 1) / 3 * 3 + 1, 1)
        }
        TimeUnit::Year => {
            let (y, _, _) = timestamp_to_date(ts);
            date_to_timestamp(y, 1, 1)
        }
    }
}

/// time/startOf - start of a time period.
pub struct TimeStartOfOp;
impl Operator for TimeStartOfOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/startOf", >= 2);
        let ts = eval_expr(&args[0], ctx)?.to_number().unwrap_or(0.0);
        let unit = TimeUnit::parse(&eval_expr(&args[1], ctx)?.to_string_value())?;
        Ok(Value::Number(start_of(ts, unit)))
    }
}

/// time/endOf - last millisecond of a time period.
pub struct TimeEndOfOp;
impl Operator for TimeEndOfOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/endOf", >= 2);
        let ts = eval_expr(&args[0], ctx)?.to_number().unwrap_or(0.0);
        let unit = TimeUnit::parse(&eval_expr(&args[1], ctx)?.to_string_value())?;
        let day_ms = 24.0 * 60.0 * 60.0 * 1000.0;
        let result = match unit {
            TimeUnit::Millisecond => ts.floor(),
            TimeUnit::Second => ((ts / 1000.0).floor() + 1.0) * 1000.0 - 1.0,
            TimeUnit::Minute => ((ts / 60_000.0).floor() + 1.0) * 60_000.0 - 1.0,
            TimeUnit::Hour => ((ts / 3_600_000.0).floor() + 1.0) * 3_600_000.0 - 1.0,
            TimeUnit::Day => ((ts / day_ms).floor() + 1.0) * day_ms - 1.0,
            TimeUnit::Week => {
                let days = (ts / day_ms).floor() as i64;
                let dow = (days + 3).rem_euclid(7);
                (((days - dow) + 7) as f64) * day_ms - 1.0
            }
            TimeUnit::Month => {
                let (y, m, _) = timestamp_to_date(ts);
                let (ny, nm) = if m >= 12 { (y + 1, 1) } else { (y, m + 1) };
                date_to_timestamp(ny, nm, 1) - 1.0
            }
            TimeUnit::Quarter => {
                let (y, m, _) = timestamp_to_date(ts);
                let next = (m - 1) / 3 * 3 + 4;
                let (ny, nm) = if next > 12 {
                    (y + 1, next - 12)
                } else {
                    (y, next)
                };
                date_to_timestamp(ny, nm, 1) - 1.0
            }
            TimeUnit::Year => {
                let (y, _, _) = timestamp_to_date(ts);
                date_to_timestamp(y + 1, 1, 1) - 1.0
            }
        };
        Ok(Value::Number(result))
    }
}

/// time/relative - human relative time ("2 hours ago", "in 3 days").
pub struct TimeRelativeOp;
impl Operator for TimeRelativeOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/relative", 1);
        let ts = eval_expr(&args[0], ctx)?.to_number().unwrap_or(0.0);
        let now = ctx.now_millis();
        let diff = now - ts; // >0 past, <0 future
        let plural = |n: i64| if n == 1 { "" } else { "s" };
        let phrase = |n: i64, unit: &str| {
            if diff < 0.0 {
                format!("in {} {}{}", n, unit, plural(n))
            } else {
                format!("{} {}{} ago", n, unit, plural(n))
            }
        };
        // Same buckets as the runtime evaluator (`evalTimeRelative`) and the compiled TS emit.
        let ms = diff.abs();
        let (minute, hour, day) = (60_000.0, 3_600_000.0, 86_400_000.0);
        let out = if ms < minute {
            "just now".to_string()
        } else if ms < hour {
            phrase((ms / minute).round() as i64, "minute")
        } else if ms < day {
            phrase((ms / hour).round() as i64, "hour")
        } else if ms < 7.0 * day {
            phrase((ms / day).round() as i64, "day")
        } else if ms < 30.0 * day {
            phrase((ms / (7.0 * day)).round() as i64, "week")
        } else if ms < 365.0 * day {
            phrase((ms / (30.0 * day)).round() as i64, "month")
        } else {
            phrase((ms / (365.0 * day)).round() as i64, "year")
        };
        Ok(Value::String(out))
    }
}

/// format/creditCard - mask all but the last 4 digits, grouped in 4s.
pub struct FormatCreditCardOp;
impl Operator for FormatCreditCardOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "format/creditCard", 1);
        let digits: String = eval_expr(&args[0], ctx)?
            .to_string_value()
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect();
        if digits.len() <= 4 {
            return Ok(Value::String(digits));
        }
        let last4 = &digits[digits.len() - 4..];
        let mut groups: Vec<String> = Vec::new();
        let mask_len = digits.len() - 4;
        let mut i = 0;
        while i < mask_len {
            let g = (mask_len - i).min(4);
            groups.push("•".repeat(g));
            i += g;
        }
        groups.push(last4.to_string());
        Ok(Value::String(groups.join(" ")))
    }
}

/// Stub for the `agent/*` operators — they require the agent runtime, which is
/// not hosted in orbital-core. Keeps name parity with the canonical registry.
pub struct AgentStubOp(pub &'static str);
impl Operator for AgentStubOp {
    fn evaluate(&self, _args: &[SExpression], _ctx: &BindingContext) -> EvalResult {
        Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
            "operator '{}' requires the agent runtime",
            self.0
        ))))
    }
}
