use crate::types::Color;

const MIN_OVERHEAD_MS: i64 = 10;
const MIN_BUDGET_MS: u64 = 1;

/// Soft limit: stop between iterations. Hard limit: abort mid-iteration.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TimeBudget {
    pub soft_ms: u64,
    pub hard_ms: u64,
}

impl TimeBudget {
    /// A fixed per-move time (`go movetime`).
    pub fn fixed(ms: u64) -> Self {
        let ms = ms.max(MIN_BUDGET_MS);
        Self {
            soft_ms: ms,
            hard_ms: ms,
        }
    }
}

#[derive(Copy, Clone, Default)]
pub struct TimeControl {
    pub wtime: i64,
    pub btime: i64,
    pub winc: i64,
    pub binc: i64,
    pub movestogo: i32,
    pub move_overhead_ms: i64,
}

impl TimeControl {
    pub fn budget(&self, turn: Color) -> TimeBudget {
        let (time, inc) = if turn == Color::White {
            (self.wtime, self.winc)
        } else {
            (self.btime, self.binc)
        };

        let overhead = self.move_overhead_ms.max(MIN_OVERHEAD_MS);
        let time_left = (time - overhead).max(1);
        let inc = inc.max(0);

        let soft = if self.movestogo > 0 {
            time_left / (i64::from(self.movestogo) + 2) + inc * 3 / 4
        } else {
            time_left / 20 + inc * 3 / 4
        };
        let hard = (time_left / 4).max(soft).min(soft * 4).min(time_left);
        let soft = soft.min(hard);

        TimeBudget {
            soft_ms: (soft.max(1) as u64).max(MIN_BUDGET_MS),
            hard_ms: (hard.max(1) as u64).max(MIN_BUDGET_MS),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white_clock(time: i64, inc: i64) -> TimeControl {
        TimeControl {
            wtime: time,
            winc: inc,
            ..TimeControl::default()
        }
    }

    #[test]
    fn hard_limit_never_exceeds_remaining_time_with_large_increment() {
        let budget = white_clock(1000, 1000).budget(Color::White);
        assert!(budget.hard_ms < 1000, "{budget:?}");
        assert!(budget.soft_ms <= budget.hard_ms);
    }

    #[test]
    fn soft_limit_is_a_small_fraction_of_a_long_clock() {
        let budget = white_clock(60_000, 0).budget(Color::White);
        assert_eq!(budget.soft_ms, (60_000 - MIN_OVERHEAD_MS as u64) / 20);
        assert!(budget.hard_ms <= 60_000 / 4);
    }

    #[test]
    fn overhead_is_reserved_from_the_clock() {
        let tc = TimeControl {
            move_overhead_ms: 500,
            ..white_clock(1000, 0)
        };
        assert!(tc.budget(Color::White).hard_ms <= 500);
    }

    #[test]
    fn nearly_empty_clock_still_gives_positive_budget() {
        let budget = white_clock(5, 0).budget(Color::White);
        assert!(budget.soft_ms >= 1 && budget.hard_ms >= budget.soft_ms);
    }
}
