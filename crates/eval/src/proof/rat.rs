//! Exact rationals for the linear theory. Every operation is checked: an overflow returns `None`, and the caller
//! reports the obligation unproved rather than guessing.

use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rat {
    n: i128,
    d: i128,
}

/// Numeric order, exact for every pair (continued-fraction comparison, so no product can overflow).
impl Ord for Rat {
    fn cmp(&self, o: &Rat) -> Ordering {
        let (mut a, mut b, mut flip) = ((self.n, self.d), (o.n, o.d), false);
        loop {
            let (qa, ra) = (a.0.div_euclid(a.1), a.0.rem_euclid(a.1));
            let (qb, rb) = (b.0.div_euclid(b.1), b.0.rem_euclid(b.1));
            let ord = match (qa.cmp(&qb), ra == 0, rb == 0) {
                (Ordering::Equal, true, true) => return Ordering::Equal,
                (Ordering::Equal, true, false) => Ordering::Less,
                (Ordering::Equal, false, true) => Ordering::Greater,
                (Ordering::Equal, false, false) => {
                    (a, b, flip) = ((a.1, ra), (b.1, rb), !flip);
                    continue;
                }
                (other, _, _) => other,
            };
            return if flip { ord.reverse() } else { ord };
        }
    }
}

impl PartialOrd for Rat {
    fn partial_cmp(&self, o: &Rat) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl Rat {
    pub const ZERO: Rat = Rat { n: 0, d: 1 };
    pub const ONE: Rat = Rat { n: 1, d: 1 };

    pub fn int(n: i128) -> Rat {
        Rat { n, d: 1 }
    }

    pub fn new(n: i128, d: i128) -> Option<Rat> {
        if d == 0 {
            return None;
        }
        let (n, d) = if d < 0 {
            (n.checked_neg()?, d.checked_neg()?)
        } else {
            (n, d)
        };
        let g = gcd(n.unsigned_abs(), d.unsigned_abs());
        let g = i128::try_from(g.max(1)).ok()?;
        Some(Rat { n: n / g, d: d / g })
    }

    /// The exact value of the shortest decimal that round-trips `x`, so a literal `0.1` is one tenth.
    pub fn from_f64(x: f64) -> Option<Rat> {
        if !x.is_finite() {
            return None;
        }
        parse_decimal(&format!("{x:?}"))
    }

    pub fn to_f64(self) -> f64 {
        self.n as f64 / self.d as f64
    }

    pub fn is_zero(self) -> bool {
        self.n == 0
    }

    pub fn is_negative(self) -> bool {
        self.n < 0
    }

    pub fn is_positive(self) -> bool {
        self.n > 0
    }

    pub fn checked_negate(self) -> Option<Rat> {
        Some(Rat {
            n: self.n.checked_neg()?,
            d: self.d,
        })
    }

    pub fn checked_plus(self, o: Rat) -> Option<Rat> {
        let n = self
            .n
            .checked_mul(o.d)?
            .checked_add(o.n.checked_mul(self.d)?)?;
        Rat::new(n, self.d.checked_mul(o.d)?)
    }

    pub fn checked_minus(self, o: Rat) -> Option<Rat> {
        self.checked_plus(o.checked_negate()?)
    }

    pub fn checked_times(self, o: Rat) -> Option<Rat> {
        Rat::new(self.n.checked_mul(o.n)?, self.d.checked_mul(o.d)?)
    }

    pub fn checked_over(self, o: Rat) -> Option<Rat> {
        Rat::new(self.n.checked_mul(o.d)?, self.d.checked_mul(o.n)?)
    }
}

fn parse_decimal(text: &str) -> Option<Rat> {
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i32>().ok()?),
        None => (text, 0),
    };
    let (negative, digits) = match mantissa.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, mantissa),
    };
    let (whole, frac) = digits.split_once('.').unwrap_or((digits, ""));
    let mut n: i128 = 0;
    for c in whole.chars().chain(frac.chars()) {
        n = n
            .checked_mul(10)?
            .checked_add(i128::from(c.to_digit(10)?))?;
    }
    let scale = exponent - i32::try_from(frac.len()).ok()?;
    let ten = |k: u32| 10i128.checked_pow(k);
    let r = if scale >= 0 {
        Rat::new(n.checked_mul(ten(scale.unsigned_abs())?)?, 1)?
    } else {
        Rat::new(n, ten(scale.unsigned_abs())?)?
    };
    if negative {
        r.checked_negate()
    } else {
        Some(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_literals_are_exact() {
        assert_eq!(Rat::from_f64(0.1), Rat::new(1, 10));
        assert_eq!(Rat::from_f64(-2.5), Rat::new(-5, 2));
        assert_eq!(Rat::from_f64(1e21), Some(Rat::int(10i128.pow(21))));
        assert_eq!(Rat::from_f64(1.5e-7), Rat::new(15, 100_000_000));
        assert_eq!(Rat::from_f64(f64::NAN), None);
    }

    #[test]
    fn arithmetic_normalizes_and_overflow_is_none() {
        let third = Rat::new(1, 3).unwrap();
        assert_eq!(
            third
                .checked_plus(third)
                .and_then(|x| x.checked_plus(third)),
            Some(Rat::ONE)
        );
        assert_eq!(Rat::new(2, -4), Rat::new(-1, 2));
        assert_eq!(Rat::int(i128::MAX).checked_plus(Rat::ONE), None);
        assert_eq!(Rat::ONE.checked_over(Rat::ZERO), None);
    }

    #[test]
    fn order_is_numeric_including_negatives_and_huge_terms() {
        let r = |n, d| Rat::new(n, d).unwrap();
        assert!(r(1, 2) < r(2, 3));
        assert!(r(-1, 2) < r(-1, 3));
        assert!(r(-3, 2) < r(-1, 1));
        assert!(r(7, 3) > r(2, 1));
        assert_eq!(r(4, 6).cmp(&r(2, 3)), Ordering::Equal);
        let big = i128::MAX / 3;
        assert!(r(big, big - 1) < r(big - 1, big - 2));
        assert!(Rat::ONE < r(big, big - 1));
        assert!(r(big - 1, big) < r(big, big + 1));
        assert!(r(1, 3) > r(0, 1) && r(-1, 3) < Rat::ZERO);
    }
}
