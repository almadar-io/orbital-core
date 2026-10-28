//! Time operators: now, arithmetic, parsing and formatting of instants.

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

pub(super) fn days_in_month(year: i32, month: i32) -> i32 {
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
pub(super) fn add_calendar_months(ts: f64, months: i64) -> f64 {
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
pub(super) fn whole_months_between(from: f64, to: f64) -> i64 {
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
pub(super) fn shift_time(ts: f64, amount: f64, unit: TimeUnit) -> f64 {
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
pub(super) fn parse_iso_instant(text: &str) -> Option<i64> {
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

/// time/parse - RFC 3339 / ISO 8601 date or date-time to epoch ms; a date/datetime value already held as epoch ms is
/// that number; anything else answers null.
pub struct TimeParseOp;
impl Operator for TimeParseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/parse", 1);

        let date_str = match eval_expr(&args[0], ctx)? {
            Value::String(s) => s,
            Value::Number(ms) => {
                return Ok(if ms.is_finite() {
                    Value::Number(ms)
                } else {
                    Value::Null
                })
            }
            other => {
                return Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
                    "time/parse: expected a string argument, got {}",
                    other.type_name()
                ))))
            }
        };
        match parse_iso_instant(&date_str) {
            Some(ms) => Ok(Value::Number(ms as f64)),
            None => Ok(Value::Null),
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
pub(super) fn format_timestamp_tokens(ts_ms: f64, format: &str) -> Option<String> {
    if !ts_ms.is_finite() {
        return None;
    }
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ts_ms as i64)?;
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
    Some(out)
}

/// time/format - Format timestamp to string (basic support)
pub struct TimeFormatOp;
impl Operator for TimeFormatOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "time/format", 1);

        let ts = require_number!(eval_expr(&args[0], ctx)?);

        if let Some(format_expr) = args.get(1) {
            let format = eval_expr(format_expr, ctx)?.to_string_value();
            return Ok(format_timestamp_tokens(ts, &format).map_or(Value::Null, Value::string));
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

        Ok(Value::string(format!(
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

pub(super) fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
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
pub(super) fn estimate_year_from_days(total_days: i64) -> i32 {
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

pub(super) fn count_leap_years(from: i32, to: i32) -> i32 {
    let count_leaps = |y: i32| -> i32 { (y / 4) - (y / 100) + (y / 400) };
    count_leaps(to - 1) - count_leaps(from - 1)
}

pub(super) fn timestamp_to_date(ts: f64) -> (i32, i32, i32) {
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

pub(super) fn days_since_epoch(year: i32) -> i64 {
    let years = year - 1970;
    let leap_years = count_leap_years(1970, year);
    (years as i64) * 365 + (leap_years as i64)
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

        Ok(Value::string(parts.join(" ")))
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
