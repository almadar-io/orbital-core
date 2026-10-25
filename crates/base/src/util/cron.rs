//! Cron expression parsing and matching.
//!
//! The canonical Rust port of `packages/almadar-runtime/src/createOsHandlers.ts`'s
//! `parseCronField`/`parseCron`/`cronMatches` — ported field-for-field (not
//! reimplemented against a new grammar, and not pulled in via an off-the-shelf
//! cron crate) so every engine that schedules a cron-shaped tick interval
//! agrees bit-for-bit with the one that already runs `os/watch-cron` today.
//!
//! Used by:
//! - `orbital-compiler::phases::resolve::resolve_tick` — reject a tick's
//!   interval string at compile time unless it's a valid duration/frame/cron.
//! - `orbital-shell-typescript::codegen::effect::tick` — emit real
//!   cron-matching code for a `ticks { ... every "<cron>" }` block.
//! - `orbital-server::tick::scheduler` — the native Rust tick scheduler.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::collections::HashSet;

/// The five parsed cron fields, each a set of matching values in its own range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronFields {
    pub minute: HashSet<u32>,
    pub hour: HashSet<u32>,
    pub day: HashSet<u32>,
    pub month: HashSet<u32>,
    pub weekday: HashSet<u32>,
}

/// Parse one cron field (`*`, `N`, `N-M`, `N,M,...`, or `*/N` / `N-M/N`) into
/// the set of values it matches within `[min, max]`. Mirrors
/// `createOsHandlers.ts`'s `parseCronField` exactly.
fn parse_cron_field(field: &str, min: u32, max: u32) -> Result<HashSet<u32>, String> {
    let mut values = HashSet::new();
    for part in field.split(',') {
        if part == "*" {
            values.extend(min..=max);
        } else if let Some((range, step_str)) = part.split_once('/') {
            let step: u32 = step_str
                .parse()
                .map_err(|_| format!("invalid cron step '{part}'"))?;
            if step == 0 {
                return Err(format!("invalid cron step '{part}': step cannot be 0"));
            }
            let start: u32 = if range == "*" {
                min
            } else {
                range
                    .parse()
                    .map_err(|_| format!("invalid cron range start '{part}'"))?
            };
            let mut i = start;
            while i <= max {
                values.insert(i);
                i += step;
            }
        } else if let Some((lo, hi)) = part.split_once('-') {
            let lo: u32 = lo
                .parse()
                .map_err(|_| format!("invalid cron range '{part}'"))?;
            let hi: u32 = hi
                .parse()
                .map_err(|_| format!("invalid cron range '{part}'"))?;
            values.extend(lo..=hi);
        } else {
            let value: u32 = part
                .parse()
                .map_err(|_| format!("invalid cron field value '{part}'"))?;
            values.insert(value);
        }
    }
    Ok(values)
}

/// Parse a full 5-field cron expression (`minute hour day month weekday`).
/// Mirrors `createOsHandlers.ts`'s `parseCron` — same field count, order,
/// and per-field ranges (minute 0-59, hour 0-23, day 1-31, month 1-12,
/// weekday 0-6).
pub fn parse_cron(expression: &str) -> Result<CronFields, String> {
    let parts: Vec<&str> = expression.split_whitespace().collect();
    if parts.len() != 5 {
        return Err(format!(
            "invalid cron expression (expected 5 fields): {expression}"
        ));
    }
    Ok(CronFields {
        minute: parse_cron_field(parts[0], 0, 59)?,
        hour: parse_cron_field(parts[1], 0, 23)?,
        day: parse_cron_field(parts[2], 1, 31)?,
        month: parse_cron_field(parts[3], 1, 12)?,
        weekday: parse_cron_field(parts[4], 0, 6)?,
    })
}

/// Returns true if `expression` is a syntactically valid 5-field cron
/// expression, without needing the parsed field sets. Cheap shape check for
/// callers (like the compiler's tick-interval validation) that only need a
/// yes/no answer.
pub fn is_valid_cron_expression(expression: &str) -> bool {
    parse_cron(expression).is_ok()
}

/// Whether `fields` matches the given calendar moment. `weekday` is
/// 0 (Sunday) through 6 (Saturday), matching JS `Date.getDay()` and
/// `chrono::Datelike::weekday().num_days_from_sunday()`.
pub fn cron_matches(
    fields: &CronFields,
    minute: u32,
    hour: u32,
    day: u32,
    month: u32,
    weekday: u32,
) -> bool {
    fields.minute.contains(&minute)
        && fields.hour.contains(&hour)
        && fields.day.contains(&day)
        && fields.month.contains(&month)
        && fields.weekday.contains(&weekday)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_field_count() {
        assert!(parse_cron("* * *").is_err());
        assert!(parse_cron("0 9 * * * *").is_err());
    }

    #[test]
    fn every_minute_matches_everything() {
        let f = parse_cron("* * * * *").unwrap();
        assert!(cron_matches(&f, 0, 0, 1, 1, 0));
        assert!(cron_matches(&f, 59, 23, 31, 12, 6));
    }

    #[test]
    fn daily_at_9am_matches_only_9_00() {
        let f = parse_cron("0 9 * * *").unwrap();
        assert!(cron_matches(&f, 0, 9, 15, 6, 3));
        assert!(!cron_matches(&f, 1, 9, 15, 6, 3));
        assert!(!cron_matches(&f, 0, 10, 15, 6, 3));
    }

    #[test]
    fn step_and_range_and_list() {
        let f = parse_cron("*/15 9-17 * * 1,3,5").unwrap();
        assert!(f.minute.contains(&0));
        assert!(f.minute.contains(&45));
        assert!(!f.minute.contains(&10));
        assert!(f.hour.contains(&9));
        assert!(f.hour.contains(&17));
        assert!(!f.hour.contains(&8));
        assert_eq!(f.weekday, HashSet::from([1, 3, 5]));
    }

    #[test]
    fn is_valid_cron_expression_matches_parse_cron() {
        assert!(is_valid_cron_expression("0 9 * * *"));
        assert!(is_valid_cron_expression("*/5 * * * *"));
        assert!(!is_valid_cron_expression("not a cron"));
        assert!(!is_valid_cron_expression("30s"));
        assert!(!is_valid_cron_expression("frame"));
    }
}
