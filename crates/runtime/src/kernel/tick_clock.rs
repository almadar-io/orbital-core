//! The one tick due-computation: decides which declared ticks are due on a host frame.
//!
//! Mirrors `packages/almadar-runtime/src/time/TickScheduler.ts` as `useClientTicks` drives it on
//! web: ONE accumulator pass per host frame; an interval tick fires at most once per pass (missed
//! beats are dropped, the phase remainder is kept, so there is never a catch-up burst); an
//! interval of `0` / `"frame"` fires on every pass; a cron tick is checked at most once a second
//! and fires at most once per matching calendar minute; `pause`/`resume` stop the clock and reset
//! every tick's timestamp (not its accumulated phase) so the paused span is never counted.
//!
//! Pure: time comes in as `now_ms`, calendar time as a closure. Every host loop (CADisplayLink /
//! `withFrameNanos` / the egui and dioxus frame hooks) differences its own monotonic clock into
//! `now_ms`; this module never touches the kernel.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::cron::{cron_matches, parse_cron, CronFields};
use orbital_core_base::schema::types::{Orbital, OrbitalSchema, StringOrNumber, TraitReference};
use std::collections::BTreeSet;
use std::sync::Arc;

/// Cron ticks are re-checked at most this often (calendar-minute resolution is enough).
const CRON_CHECK_INTERVAL_MS: u64 = 1000;

/// One calendar moment, in the viewer's local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Calendar {
    pub minute: u32,
    pub hour: u32,
    pub day: u32,
    pub month: u32,
    /// 0 = Sunday .. 6 = Saturday.
    pub weekday: u32,
    /// Unique per calendar minute; dedups a match across the seconds of that minute.
    pub minute_key: i64,
}

impl Calendar {
    /// A bare-metal host carries no time-zone database: its calendar is UTC, read from the host clock.
    #[cfg(all(not(feature = "std"), not(target_arch = "wasm32")))]
    pub fn local_now() -> Self {
        use chrono::{Datelike, Timelike};
        let now = chrono::DateTime::from_timestamp_millis(orbital_core_base::time_util::now_millis())
            .expect("the host clock answers a representable instant");
        Self {
            minute: now.minute(),
            hour: now.hour(),
            day: now.day(),
            month: now.month(),
            weekday: now.weekday().num_days_from_sunday(),
            minute_key: i64::from(now.year()) * 100_000_000
                + i64::from(now.month0()) * 1_000_000
                + i64::from(now.day()) * 10_000
                + i64::from(now.hour()) * 100
                + i64::from(now.minute()),
        }
    }

    #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
    pub fn local_now() -> Self {
        use chrono::{Datelike, Local, Timelike};
        let now = Local::now();
        Self {
            minute: now.minute(),
            hour: now.hour(),
            day: now.day(),
            month: now.month(),
            weekday: now.weekday().num_days_from_sunday(),
            minute_key: i64::from(now.year()) * 100_000_000
                + i64::from(now.month0()) * 1_000_000
                + i64::from(now.day()) * 10_000
                + i64::from(now.hour()) * 100
                + i64::from(now.minute()),
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn local_now() -> Self {
        let now = js_sys::Date::new_0();
        let (year, month0, day) = (
            i64::from(now.get_full_year()),
            i64::from(now.get_month()),
            i64::from(now.get_date()),
        );
        let (hour, minute) = (i64::from(now.get_hours()), i64::from(now.get_minutes()));
        Self {
            minute: now.get_minutes(),
            hour: now.get_hours(),
            day: now.get_date(),
            month: now.get_month() + 1,
            weekday: now.get_day(),
            minute_key: year * 100_000_000
                + month0 * 1_000_000
                + day * 10_000
                + hour * 100
                + minute,
        }
    }
}

enum Schedule {
    EveryPass,
    Interval {
        interval_ms: u64,
        accumulated_ms: u64,
    },
    Cron {
        fields: Box<CronFields>,
        accumulated_ms: u64,
        last_fired_minute_key: i64,
    },
}

struct Entry {
    trait_name: String,
    tick_name: String,
    schedule: Schedule,
    last_ms: Option<u64>,
    while_visible: bool,
}

/// Every declared tick of a schema plus the phase bookkeeping of its interval.
pub struct TickClock {
    entries: Vec<Entry>,
    paused: bool,
    /// Traits the host reports off screen; a trait it never reported counts as on screen.
    off_screen: BTreeSet<String>,
}

impl TickClock {
    /// One entry per `(trait, tick)` declared by an inline trait, in declaration order. A trait
    /// name declared by two orbitals is registered once (first wins), as the kernel keys
    /// trait definitions by name.
    pub fn from_schema(schema: &OrbitalSchema) -> Self {
        Self::from_orbitals(&schema.orbitals)
    }

    /// [`Self::from_schema`] over a subset of orbitals (a client scoped to its active orbital).
    pub fn from_orbitals(orbitals: &[Orbital]) -> Self {
        Self::from_orbital_iter(orbitals.iter())
    }

    /// [`Self::from_schema`] over every program given: the booted one and the lazy ones loaded with it.
    pub fn from_programs(programs: &[Arc<OrbitalSchema>]) -> Self {
        Self::from_orbital_iter(programs.iter().flat_map(|p| p.orbitals.iter()))
    }

    fn from_orbital_iter<'a>(orbitals: impl Iterator<Item = &'a Orbital>) -> Self {
        let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
        let mut entries = Vec::new();
        for orbital in orbitals {
            for reference in &orbital.traits {
                let TraitReference::Inline(def) = reference else {
                    continue;
                };
                for tick in def.ticks.iter().flatten() {
                    if !seen.insert((def.name.clone(), tick.name.clone())) {
                        continue;
                    }
                    let cron = match &tick.interval {
                        StringOrNumber::String(s) => parse_cron(s).ok(),
                        StringOrNumber::Number(_) => None,
                    };
                    let schedule = match (cron, tick.interval_ms()) {
                        (Some(fields), _) => Schedule::Cron {
                            fields: Box::new(fields),
                            accumulated_ms: CRON_CHECK_INTERVAL_MS,
                            last_fired_minute_key: -1,
                        },
                        (None, None | Some(0)) => Schedule::EveryPass,
                        (None, Some(interval_ms)) => Schedule::Interval {
                            interval_ms,
                            accumulated_ms: 0,
                        },
                    };
                    entries.push(Entry {
                        trait_name: def.name.clone(),
                        tick_name: tick.name.clone(),
                        schedule,
                        last_ms: None,
                        while_visible: tick.while_visible,
                    });
                }
            }
        }
        Self {
            entries,
            paused: false,
            off_screen: BTreeSet::new(),
        }
    }

    /// The host's report of whether `trait_name`'s rendered output is on screen. A `[visible]`
    /// tick of an off-screen trait keeps its schedule but its fires are dropped (LOLO §11, the
    /// web's `useClientTicks` `offScreen`).
    pub fn set_on_screen(&mut self, trait_name: &str, on_screen: bool) {
        if on_screen {
            self.off_screen.remove(trait_name);
        } else {
            self.off_screen.insert(trait_name.to_string());
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Stop firing; registered ticks and their phase are kept. A no-op if already paused.
    pub fn pause(&mut self) {
        self.paused = true;
    }

    /// Resume after `pause`: every tick's timestamp is cleared (so the next pass only captures
    /// it), the accumulated phase is kept. A no-op if not paused.
    pub fn resume(&mut self) {
        if !self.paused {
            return;
        }
        self.paused = false;
        for entry in &mut self.entries {
            entry.last_ms = None;
        }
    }

    /// The `(trait, tick)` pairs due on this pass, in declaration order. `calendar` is read only
    /// when a cron tick reaches its once-a-second check.
    pub fn due(
        &mut self,
        now_ms: u64,
        calendar: impl FnMut() -> Calendar,
    ) -> Vec<(String, String)> {
        self.due_among(now_ms, calendar, None)
    }

    /// [`Self::due`] over the ticks of `mounted` traits only (`None`: every trait). An unmounted
    /// trait's ticks neither fire nor accumulate; its timestamp is cleared, so a remount only
    /// captures it on the next pass.
    pub fn due_among(
        &mut self,
        now_ms: u64,
        mut calendar: impl FnMut() -> Calendar,
        mounted: Option<&BTreeSet<String>>,
    ) -> Vec<(String, String)> {
        if self.paused {
            return Vec::new();
        }
        let mut due = Vec::new();
        for entry in &mut self.entries {
            if mounted.is_some_and(|m| !m.contains(&entry.trait_name)) {
                entry.last_ms = None;
                continue;
            }
            let fire = match &mut entry.schedule {
                Schedule::EveryPass => true,
                Schedule::Interval {
                    interval_ms,
                    accumulated_ms,
                } => match entry.last_ms.replace(now_ms) {
                    None => false,
                    Some(last) => {
                        *accumulated_ms += now_ms.saturating_sub(last);
                        if *accumulated_ms >= *interval_ms {
                            *accumulated_ms %= *interval_ms;
                            true
                        } else {
                            false
                        }
                    }
                },
                Schedule::Cron {
                    fields,
                    accumulated_ms,
                    last_fired_minute_key,
                } => match entry.last_ms.replace(now_ms) {
                    None => false,
                    Some(last) => {
                        *accumulated_ms += now_ms.saturating_sub(last);
                        if *accumulated_ms < CRON_CHECK_INTERVAL_MS {
                            false
                        } else {
                            *accumulated_ms = 0;
                            let at = calendar();
                            if at.minute_key != *last_fired_minute_key
                                && cron_matches(
                                    fields, at.minute, at.hour, at.day, at.month, at.weekday,
                                )
                            {
                                *last_fired_minute_key = at.minute_key;
                                true
                            } else {
                                false
                            }
                        }
                    }
                },
            };
            if fire && !(entry.while_visible && self.off_screen.contains(&entry.trait_name)) {
                due.push((entry.trait_name.clone(), entry.tick_name.clone()));
            }
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema(ticks: serde_json::Value) -> OrbitalSchema {
        serde_json::from_value(json!({
            "name": "t",
            "version": "1.0.0",
            "typeContractVersion": 1, "orbitals": [{
                "name": "O",
                "entity": { "name": "E", "persistence": "runtime", "fields": [] },
                "traits": [{
                    "name": "T",
                    "linkedEntity": "E",
                    "stateMachine": {
                        "states": [{ "name": "idle", "isInitial": true }],
                        "events": [],
                        "transitions": []
                    },
                    "ticks": ticks
                }],
                "pages": []
            }]
        }))
        .expect("schema")
    }

    fn cal(minute_key: i64, minute: u32) -> Calendar {
        Calendar {
            minute,
            hour: 9,
            day: 1,
            month: 1,
            weekday: 1,
            minute_key,
        }
    }

    fn names(due: Vec<(String, String)>) -> Vec<String> {
        due.into_iter().map(|(_, tick)| tick).collect()
    }

    fn clock(ticks: serde_json::Value) -> TickClock {
        TickClock::from_schema(&schema(ticks))
    }

    #[test]
    fn interval_tick_fires_once_per_elapsed_interval_and_not_before() {
        let mut c = clock(json!([{ "name": "beat", "interval": 100, "effects": [] }]));
        assert!(
            c.due(1000, || cal(1, 0)).is_empty(),
            "first pass only captures the timestamp"
        );
        assert!(
            c.due(1099, || cal(1, 0)).is_empty(),
            "control: 99ms elapsed is not due"
        );
        assert_eq!(names(c.due(1100, || cal(1, 0))), ["beat"]);
        assert!(
            c.due(1150, || cal(1, 0)).is_empty(),
            "phase restarted after the fire"
        );
        assert_eq!(names(c.due(1200, || cal(1, 0))), ["beat"]);
    }

    #[test]
    fn an_overloaded_pass_fires_once_and_drops_the_missed_beats() {
        let mut c = clock(json!([{ "name": "beat", "interval": 100, "effects": [] }]));
        c.due(0, || cal(1, 0));
        assert_eq!(
            names(c.due(1050, || cal(1, 0))),
            ["beat"],
            "ten beats missed, one fire"
        );
        assert!(
            c.due(1051, || cal(1, 0)).is_empty(),
            "no backlog burst on the next pass"
        );
        assert_eq!(
            names(c.due(1100, || cal(1, 0))),
            ["beat"],
            "50ms phase remainder kept"
        );
    }

    #[test]
    fn frame_and_zero_interval_ticks_fire_on_every_pass_including_the_first() {
        let mut c = clock(json!([
            { "name": "f", "interval": "frame", "effects": [] },
            { "name": "z", "interval": 0, "effects": [] },
            { "name": "slow", "interval": 1000, "effects": [] }
        ]));
        assert_eq!(names(c.due(10, || cal(1, 0))), ["f", "z"]);
        assert_eq!(names(c.due(11, || cal(1, 0))), ["f", "z"]);
    }

    #[test]
    fn duration_strings_resolve_through_the_schema_interval() {
        let mut c = clock(json!([{ "name": "s", "interval": "2s", "effects": [] }]));
        c.due(0, || cal(1, 0));
        assert!(c.due(1999, || cal(1, 0)).is_empty());
        assert_eq!(names(c.due(2000, || cal(1, 0))), ["s"]);
    }

    #[test]
    fn pause_stops_every_tick_and_resume_never_counts_the_paused_span() {
        let mut c = clock(json!([
            { "name": "beat", "interval": 100, "effects": [] },
            { "name": "f", "interval": "frame", "effects": [] }
        ]));
        c.due(0, || cal(1, 0));
        c.pause();
        assert!(
            c.due(5000, || cal(1, 0)).is_empty(),
            "paused: nothing fires, frame ticks included"
        );
        c.resume();
        assert_eq!(
            names(c.due(10_000, || cal(1, 0))),
            ["f"],
            "resume: interval tick re-captures, no burst"
        );
        assert_eq!(
            names(c.due(10_099, || cal(1, 0))),
            ["f"],
            "only the frame tick"
        );
        assert_eq!(names(c.due(10_100, || cal(1, 0))), ["beat", "f"]);
    }

    #[test]
    fn control_without_a_pause_the_same_gap_fires_the_interval_tick() {
        let mut c = clock(json!([{ "name": "beat", "interval": 100, "effects": [] }]));
        c.due(0, || cal(1, 0));
        assert_eq!(names(c.due(10_000, || cal(1, 0))), ["beat"]);
    }

    #[test]
    fn resume_without_a_pause_does_not_reset_phase() {
        let mut c = clock(json!([{ "name": "beat", "interval": 100, "effects": [] }]));
        c.due(0, || cal(1, 0));
        c.due(60, || cal(1, 0));
        c.resume();
        assert_eq!(names(c.due(100, || cal(1, 0))), ["beat"]);
    }

    #[test]
    fn cron_tick_checks_once_a_second_and_fires_once_per_matching_minute() {
        let mut c = clock(json!([{ "name": "daily", "interval": "0 9 * * *", "effects": [] }]));
        assert!(
            c.due(0, || cal(7, 0)).is_empty(),
            "first pass captures the timestamp"
        );
        assert_eq!(
            names(c.due(10, || cal(7, 0))),
            ["daily"],
            "first check runs at once, minute 0 hour 9 matches"
        );
        assert!(
            c.due(2000, || cal(7, 0)).is_empty(),
            "same calendar minute is deduped"
        );
        assert_eq!(
            names(c.due(4000, || cal(8, 0))),
            ["daily"],
            "a later matching minute fires again"
        );
    }

    #[test]
    fn cron_tick_does_not_fire_off_the_matching_minute_and_throttles_its_check() {
        let mut c = clock(json!([{ "name": "daily", "interval": "0 9 * * *", "effects": [] }]));
        c.due(0, || cal(7, 5));
        assert!(
            c.due(10, || cal(7, 5)).is_empty(),
            "minute 5 does not match"
        );
        let mut checks = 0;
        c.due(500, || {
            checks += 1;
            cal(7, 0)
        });
        assert_eq!(
            checks, 0,
            "under a second since the last check: calendar not read"
        );
    }

    #[test]
    fn a_visible_tick_off_screen_drops_its_fires_and_keeps_its_phase() {
        let mut c = clock(
            json!([{ "name": "beat", "interval": 100, "whileVisible": true, "effects": [] }]),
        );
        c.due(0, || cal(1, 0));
        c.set_on_screen("T", false);
        assert!(
            c.due(100, || cal(1, 0)).is_empty(),
            "off screen: the due beat is dropped"
        );
        assert!(c.due(250, || cal(1, 0)).is_empty());
        c.set_on_screen("T", true);
        assert!(
            c.due(299, || cal(1, 0)).is_empty(),
            "phase kept: the next beat is at 300, not a catch-up burst"
        );
        assert_eq!(names(c.due(300, || cal(1, 0))), ["beat"]);
    }

    #[test]
    fn control_a_plain_tick_fires_off_screen() {
        let mut c = clock(json!([
            { "name": "beat", "interval": 100, "effects": [] },
            { "name": "seen", "interval": 100, "whileVisible": true, "effects": [] }
        ]));
        c.due(0, || cal(1, 0));
        c.set_on_screen("T", false);
        assert_eq!(names(c.due(100, || cal(1, 0))), ["beat"]);
    }

    #[test]
    fn edge_a_visible_tick_never_reported_counts_as_on_screen() {
        let mut c = clock(
            json!([{ "name": "f", "interval": "frame", "whileVisible": true, "effects": [] }]),
        );
        assert_eq!(names(c.due(0, || cal(1, 0))), ["f"]);
        c.set_on_screen("Other", false);
        assert_eq!(
            names(c.due(1, || cal(1, 0))),
            ["f"],
            "another trait's report does not gate it"
        );
    }

    #[test]
    fn a_trait_tick_declared_twice_is_registered_once() {
        let mut s = schema(json!([{ "name": "beat", "interval": 100, "effects": [] }]));
        let dup = s.orbitals[0].traits[0].clone();
        s.orbitals[0].traits.push(dup);
        assert_eq!(TickClock::from_schema(&s).len(), 1);
    }

    #[test]
    fn a_schema_without_ticks_has_an_empty_clock() {
        let mut c = clock(json!([]));
        assert!(c.is_empty());
        assert!(c.due(0, || cal(1, 0)).is_empty());
    }

    #[test]
    fn from_orbitals_registers_only_the_given_orbitals_ticks() {
        let mut s = schema(json!([{ "name": "beat", "interval": 100, "effects": [] }]));
        let mut second = s.orbitals[0].clone();
        second.name = "Other".into();
        if let TraitReference::Inline(def) = &mut second.traits[0] {
            std::sync::Arc::make_mut(def).name = "T2".into();
        }
        s.orbitals.push(second);
        assert_eq!(TickClock::from_schema(&s).len(), 2);
        assert_eq!(TickClock::from_orbitals(&s.orbitals[1..]).len(), 1);
        assert!(
            TickClock::from_orbitals(&[]).is_empty(),
            "control: no orbitals, no ticks"
        );
    }
}
