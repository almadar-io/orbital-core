//! The seeded mock-data generator: twin of `@almadar/core/mock`'s `random.ts`.
//!
//! Same LCG, same draw per helper, same word corpus, so a seeded mock store is the
//! same bytes on the Rust path as on the runtime and compiled paths
//! (orbital-core `tests/baked/mock-seed/dataset.seeded.json` pins it).

/// Seed every mock store starts from (`DEFAULT_MOCK_SEED` in `@almadar/runtime`).
pub const DEFAULT_MOCK_SEED: u32 = 42;

pub const MS_PER_DAY: f64 = 86_400_000.0;

const WORDS: [&str; 60] = [
    "harbor", "summit", "meridian", "atlas", "beacon", "cedar", "delta", "ember", "falcon",
    "garnet", "horizon", "indigo", "juniper", "keystone", "lantern", "meadow", "nimbus", "orchard",
    "pioneer", "quarry", "redwood", "sierra", "timber", "umber", "vista", "willow", "zenith",
    "anchor", "basalt", "canyon", "drift", "estuary", "fjord", "grove", "heath", "inlet", "jade",
    "knoll", "lagoon", "mesa", "north", "onyx", "prairie", "quartz", "ridge", "slate", "terrace",
    "upland", "valley", "wharf", "yonder", "aspen", "brook", "cove", "dune", "elm", "fern", "glen",
    "haven", "isle",
];

/// One seeded stream. `now_ms` is the clock every relative date is drawn against.
#[derive(Debug, Clone)]
pub struct MockRandom {
    state: u32,
    now_ms: f64,
}

impl MockRandom {
    pub fn new(seed: u32, now_ms: f64) -> Self {
        Self {
            state: seed,
            now_ms,
        }
    }

    pub fn now_ms(&self) -> f64 {
        self.now_ms
    }

    fn next_float(&mut self) -> f64 {
        self.state = self
            .state
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        f64::from(self.state) / 4_294_967_296.0
    }

    /// Integer in `[min, max]`.
    pub fn int(&mut self, min: f64, max: f64) -> f64 {
        (self.next_float() * (max - min + 1.0)).floor() + min
    }

    pub fn boolean(&mut self) -> bool {
        self.next_float() < 0.5
    }

    fn index(&mut self, len: usize) -> usize {
        self.int(0.0, len as f64 - 1.0) as usize
    }

    pub fn words(&mut self, count: usize) -> String {
        (0..count)
            .map(|_| WORDS[self.index(WORDS.len())])
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn email(&mut self) -> String {
        let user = self.words(1).to_lowercase();
        let domain = self.words(1).to_lowercase();
        format!("{user}@{domain}.com")
    }

    pub fn url(&mut self) -> String {
        let slug = self.words(2).to_lowercase().replace(' ', "-");
        format!("https://example.com/{slug}")
    }

    pub fn phone(&mut self) -> String {
        let area = self.int(200.0, 999.0);
        let prefix = self.int(200.0, 999.0);
        let line = self.int(0.0, 9999.0);
        format!("+1 ({area}) {prefix}-{line:04}")
    }

    pub fn uuid(&mut self) -> String {
        let mut hex = |n: usize| -> String {
            (0..n)
                .map(|_| format!("{:x}", self.int(0.0, 15.0) as u32))
                .collect()
        };
        let a = hex(8);
        let b = hex(4);
        let c = hex(3);
        let d = hex(4);
        let e = hex(12);
        format!("{a}-{b}-4{c}-{d}-{e}")
    }

    /// Milliseconds roughly `years` in the past.
    pub fn past_date(&mut self, years: f64) -> f64 {
        let max_age = years * 365.0 * 24.0 * 60.0 * 60.0 * 1000.0;
        self.now_ms - (self.next_float() * max_age).floor()
    }

    /// Milliseconds in `[-days, +days]` around now.
    pub fn straddling_date(&mut self, days: f64) -> f64 {
        let span = days * MS_PER_DAY;
        self.now_ms + ((self.next_float() * span * 2.0).floor() - span)
    }
}

/// `Date.prototype.toISOString` for epoch milliseconds.
pub fn iso_from_ms(ms: f64) -> String {
    let ms = ms as i64;
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3_600_000,
        (rem / 60_000) % 60,
        (rem / 1000) % 60,
        rem % 1000
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values `@almadar/core/mock` gives after `seedRandom(42)` for the same calls.
    #[test]
    fn matches_the_js_lcg_sequence() {
        let mut r = MockRandom::new(42, 0.0);
        let ints: Vec<f64> = (0..3).map(|_| r.int(0.0, 99.0)).collect();
        assert_eq!(ints, vec![25.0, 8.0, 57.0]);
        assert_eq!(r.words(2), "keystone timber");
        assert_eq!(r.uuid(), "071dfd7a-d912-4fee-8284-fd2d7008dda6");
        assert_eq!(r.phone(), "+1 (668) 757-7113");
    }

    #[test]
    fn control_another_seed_is_another_stream() {
        let mut r = MockRandom::new(43, 0.0);
        let ints: Vec<f64> = (0..3).map(|_| r.int(0.0, 99.0)).collect();
        assert_ne!(ints, vec![25.0, 8.0, 57.0]);
    }

    #[test]
    fn formats_iso_like_date_to_iso_string() {
        assert_eq!(iso_from_ms(0.0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_from_ms(1_780_272_000_000.0), "2026-06-01T00:00:00.000Z");
        assert_eq!(iso_from_ms(951_782_400_123.0), "2000-02-29T00:00:00.123Z");
    }
}
