use crate::types::Color;
use std::fmt;
use std::ops::{Add, AddAssign, Div, Mul, Sub, SubAssign};
use std::time::Duration;

const MIN_OVERHEAD: Millis = Millis::new(10);
const MIN_BUDGET: Millis = Millis::new(1);

/// A span of clock time in milliseconds: negative once a side has overstepped its time.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Millis(i64);

impl Millis {
    pub const ZERO: Self = Self(0);

    pub const fn new(ms: i64) -> Self {
        Self(ms)
    }

    pub const fn get(self) -> i64 {
        self.0
    }

    /// `duration`, saturating at the largest span.
    pub fn from_duration(duration: Duration) -> Self {
        Self(i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
    }

    /// The milliseconds left, none once overstepped.
    pub const fn left(self) -> u64 {
        if self.0 < 0 { 0 } else { self.0.unsigned_abs() }
    }

    /// In seconds, for display.
    pub fn seconds(self) -> f64 {
        self.0 as f64 / 1000.0
    }
}

impl Add for Millis {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

impl Sub for Millis {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self(self.0 - other.0)
    }
}

impl AddAssign for Millis {
    fn add_assign(&mut self, other: Self) {
        self.0 += other.0;
    }
}

impl SubAssign for Millis {
    fn sub_assign(&mut self, other: Self) {
        self.0 -= other.0;
    }
}

impl Mul<i64> for Millis {
    type Output = Self;

    fn mul(self, factor: i64) -> Self {
        Self(self.0 * factor)
    }
}

/// A fraction of the span, rounded toward zero.
impl Div<i64> for Millis {
    type Output = Self;

    fn div(self, divisor: i64) -> Self {
        Self(self.0 / divisor)
    }
}

/// The bare number of milliseconds.
impl fmt::Display for Millis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Soft limit: stop between iterations. Hard limit: abort mid-iteration.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TimeBudget {
    pub soft: Millis,
    pub hard: Millis,
}

impl TimeBudget {
    /// A fixed per-move time (`go movetime`).
    pub fn fixed(time: Millis) -> Self {
        let time = time.max(MIN_BUDGET);
        Self {
            soft: time,
            hard: time,
        }
    }
}

#[derive(Copy, Clone, Default)]
pub struct TimeControl {
    pub wtime: Millis,
    pub btime: Millis,
    pub winc: Millis,
    pub binc: Millis,
    pub movestogo: i32,
    pub move_overhead: Millis,
}

impl TimeControl {
    pub fn budget(&self, turn: Color) -> TimeBudget {
        let (time, inc) = if turn == Color::White {
            (self.wtime, self.winc)
        } else {
            (self.btime, self.binc)
        };

        let overhead = self.move_overhead.max(MIN_OVERHEAD);
        let time_left = (time - overhead).max(MIN_BUDGET);
        let inc = inc.max(Millis::ZERO);

        let soft = if self.movestogo > 0 {
            time_left / (i64::from(self.movestogo) + 2) + inc * 3 / 4
        } else {
            time_left / 20 + inc * 3 / 4
        };
        let hard = (time_left / 4).max(soft).min(soft * 4).min(time_left);
        let soft = soft.min(hard);

        TimeBudget {
            soft: soft.max(MIN_BUDGET),
            hard: hard.max(MIN_BUDGET),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white_clock(time: i64, inc: i64) -> TimeControl {
        TimeControl {
            wtime: Millis::new(time),
            winc: Millis::new(inc),
            ..TimeControl::default()
        }
    }

    #[test]
    fn hard_limit_never_exceeds_remaining_time_with_large_increment() {
        let budget = white_clock(1000, 1000).budget(Color::White);
        assert!(budget.hard < Millis::new(1000), "{budget:?}");
        assert!(budget.soft <= budget.hard);
    }

    #[test]
    fn soft_limit_is_a_small_fraction_of_a_long_clock() {
        let budget = white_clock(60_000, 0).budget(Color::White);
        assert_eq!(budget.soft, (Millis::new(60_000) - MIN_OVERHEAD) / 20);
        assert!(budget.hard <= Millis::new(60_000 / 4));
    }

    #[test]
    fn overhead_is_reserved_from_the_clock() {
        let tc = TimeControl {
            move_overhead: Millis::new(500),
            ..white_clock(1000, 0)
        };
        assert!(tc.budget(Color::White).hard <= Millis::new(500));
    }

    #[test]
    fn nearly_empty_clock_still_gives_positive_budget() {
        let budget = white_clock(5, 0).budget(Color::White);
        assert!(budget.soft >= MIN_BUDGET && budget.hard >= budget.soft);
    }
}
