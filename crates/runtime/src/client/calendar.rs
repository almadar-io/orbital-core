//! Native calendar computation shared by every backend's `calendar-grid` body.
//!
//! The React reference (`CalendarGrid.tsx`) derives the grid from `weekStart` +
//! `dayWindow` + `timeSlots` + `events`; the `.orb` never supplies a precomputed
//! `days` array. Native backends must compute the same derivation here instead of
//! rendering empty — one owner so egui / dioxus / terminal never drift.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::value::ValueMap;
use super::helpers::{get_array, get_number, get_string, get_string_array};
use chrono::{Datelike, Duration, NaiveDate};
use orbital_core_base::value::Value;

/// One day column of the grid.
#[derive(Debug, Clone)]
pub struct CalendarDay {
    /// ISO date `YYYY-MM-DD` — matched against event `startTime` prefixes.
    pub iso: String,
    /// Short weekday name ("Mon").
    pub weekday: String,
    /// Short display label ("Jan 1").
    pub label: String,
}

/// Resolve `weekStart`: parse the prop's ISO date, else the Monday of the current
/// week (mirrors `getStartOfWeek(new Date())` in the React reference).
pub fn calendar_week_start(week_start: Option<&str>) -> NaiveDate {
    let parsed = week_start.and_then(|s| {
        let end = 10.min(s.len());
        NaiveDate::parse_from_str(&s[..end], "%Y-%m-%d").ok()
    });
    match parsed {
        Some(d) => d,
        None => {
            let today = chrono::Local::now().date_naive();
            today - Duration::days(today.weekday().num_days_from_monday() as i64)
        }
    }
}

/// The ordered day columns: `window` consecutive days starting at `start`.
pub fn calendar_days(start: NaiveDate, window: usize) -> Vec<CalendarDay> {
    (0..window.max(1))
        .map(|i| {
            let d = start + Duration::days(i as i64);
            CalendarDay {
                iso: d.format("%Y-%m-%d").to_string(),
                weekday: d.format("%a").to_string(),
                label: d.format("%b %-d").to_string(),
            }
        })
        .collect()
}

/// Resolve the visible day window: explicit number clamped to 1..=7, else 7.
/// React's `'auto'` tracks the viewport; native shells render the full week —
/// the deterministic default.
pub fn calendar_day_window(props: &Value) -> usize {
    match get_number(props, "dayWindow") {
        Some(n) => (n as usize).clamp(1, 7),
        None => 7,
    }
}

/// Time slot labels: explicit `timeSlots`, else the React default 09:00–17:00 hourly.
pub fn calendar_time_slots(props: &Value) -> Vec<String> {
    let explicit = get_string_array(props, "timeSlots");
    if !explicit.is_empty() {
        return explicit;
    }
    (9..=17).map(|h| format!("{h:02}:00")).collect()
}

/// The events to place: the resolved `events` binding (array) — falls back to
/// `items`/`data` like every other data pattern.
pub fn calendar_events(props: &Value) -> Vec<Value> {
    let ev = get_array(props, "events");
    if !ev.is_empty() {
        return ev;
    }
    super::helpers::get_explicit_rows(props)
}

/// Index an event into its day column by matching the `startTime` ISO date prefix.
pub fn event_day_index(event: &Value, days: &[CalendarDay]) -> Option<usize> {
    let start = get_string(event, "startTime")?;
    let end = 10.min(start.len());
    days.iter().position(|d| d.iso == start[..end])
}

/// The slot row for an event, by matching its `startTime` `HH:MM` against the slot
/// labels. `None` when the time doesn't align — the event still shows in its day.
pub fn event_slot_index(event: &Value, slots: &[String]) -> Option<usize> {
    let start = get_string(event, "startTime")?;
    if start.len() < 16 {
        return None;
    }
    let hhmm = &start[11..16];
    slots.iter().position(|s| s == hhmm)
}

/// Build a click payload `{date, time?}` mirroring the React callbacks
/// `onSlotClick(day, time)` / `onDayClick(day)`.
pub fn calendar_click_payload(date: &str, time: Option<&str>) -> Value {
    let mut o = ValueMap::default();
    o.insert("date".to_string(), Value::string(date.to_string()));
    if let Some(t) = time {
        o.insert("time".to_string(), Value::string(t.to_string()));
    }
    Value::object(o)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::helpers::from_json;

    fn props(s: &str) -> Value {
        from_json(serde_json::from_str(s).unwrap())
    }

    #[test]
    fn test_week_start_parses_iso() {
        let d = calendar_week_start(Some("2024-01-01"));
        assert_eq!(d, NaiveDate::from_ymd_opt(2024, 1, 1).unwrap());
        // datetime strings parse on their date prefix
        let d = calendar_week_start(Some("2024-06-15T09:00:00Z"));
        assert_eq!(d, NaiveDate::from_ymd_opt(2024, 6, 15).unwrap());
    }

    #[test]
    fn test_calendar_days_labels() {
        let days = calendar_days(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(), 3);
        assert_eq!(days.len(), 3);
        assert_eq!(days[0].iso, "2024-01-01");
        assert_eq!(days[0].weekday, "Mon");
        assert_eq!(days[0].label, "Jan 1");
        assert_eq!(days[2].iso, "2024-01-03");
        assert_eq!(days[2].weekday, "Wed");
    }

    #[test]
    fn test_day_window() {
        assert_eq!(calendar_day_window(&props(r#"{"dayWindow": 3}"#)), 3);
        assert_eq!(calendar_day_window(&props(r#"{"dayWindow": 99}"#)), 7);
        assert_eq!(calendar_day_window(&props(r#"{"dayWindow": "auto"}"#)), 7);
        assert_eq!(calendar_day_window(&props(r#"{}"#)), 7);
    }

    #[test]
    fn test_time_slots_default_and_explicit() {
        let slots = calendar_time_slots(&props(r#"{}"#));
        assert_eq!(slots, vec!["09:00", "10:00", "11:00", "12:00", "13:00", "14:00", "15:00", "16:00", "17:00"]);
        let slots = calendar_time_slots(&props(r#"{"timeSlots": ["08:00", "12:00"]}"#));
        assert_eq!(slots, vec!["08:00", "12:00"]);
    }

    #[test]
    fn test_event_placement() {
        let days = calendar_days(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(), 7);
        let slots = calendar_time_slots(&props(r#"{}"#));
        let ev = props(r#"{"title": "Standup", "startTime": "2024-01-03T10:00:00"}"#);
        assert_eq!(event_day_index(&ev, &days), Some(2));
        assert_eq!(event_slot_index(&ev, &slots), Some(1));
        let outside = props(r#"{"title": "X", "startTime": "2023-12-31T10:00:00"}"#);
        assert_eq!(event_day_index(&outside, &days), None);
    }

    #[test]
    fn test_click_payload() {
        let p = calendar_click_payload("2024-01-01", Some("09:00"));
        let o = p.as_object().unwrap();
        assert_eq!(o.get("date").and_then(|v| v.as_str()), Some("2024-01-01"));
        assert_eq!(o.get("time").and_then(|v| v.as_str()), Some("09:00"));
        let p = calendar_click_payload("2024-01-01", None);
        assert!(p.as_object().unwrap().get("time").is_none());
    }
}
