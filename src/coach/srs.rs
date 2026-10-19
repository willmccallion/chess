//! Spaced repetition: each item comes back later the more often it was got right in a row.

/// Seconds since the Unix epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnixTime(pub u64);

impl UnixTime {
    pub const fn plus_seconds(self, seconds: u64) -> Self {
        Self(self.0 + seconds)
    }

    /// The day number, counted in UTC, for grouping into days.
    pub const fn day(self) -> u64 {
        self.0 / DAY
    }
}

const MINUTE: u64 = 60;
const DAY: u64 = 24 * 60 * MINUTE;
/// How long after each result an item is due again, by box. A miss drops to box 0 and the
/// item returns within minutes; each success moves it one box up.
const BOX_INTERVALS: [u64; 7] = [10 * MINUTE, DAY, 3 * DAY, 7 * DAY, 16 * DAY, 35 * DAY, 90 * DAY];
/// Items in this box or above count as learned.
const LEARNED_BOX: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attempt {
    pub at: UnixTime,
    pub success: bool,
}

/// Where an item stands after its attempts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Schedule {
    pub level: usize,
    pub due: UnixTime,
}

impl Schedule {
    pub fn is_due(self, now: UnixTime) -> bool {
        self.due <= now
    }

    pub const fn is_learned(self) -> bool {
        self.level >= LEARNED_BOX
    }
}

/// The schedule after `attempts` in time order; None for an item never tried.
pub fn schedule(attempts: &[Attempt]) -> Option<Schedule> {
    let mut level = 0;
    let mut due = None;
    for attempt in attempts {
        level = if attempt.success {
            if due.is_none() { 1 } else { (level + 1).min(BOX_INTERVALS.len() - 1) }
        } else {
            0
        };
        due = Some(attempt.at.plus_seconds(BOX_INTERVALS[level]));
    }
    due.map(|due| Schedule { level, due })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt(day: u64, success: bool) -> Attempt {
        Attempt { at: UnixTime(day * DAY), success }
    }

    #[test]
    fn an_untried_item_has_no_schedule() {
        assert_eq!(schedule(&[]), None);
    }

    #[test]
    fn each_success_waits_longer() {
        let once = schedule(&[attempt(0, true)]).unwrap();
        let twice = schedule(&[attempt(0, true), attempt(1, true)]).unwrap();

        assert_eq!(once.due, UnixTime(DAY));
        assert_eq!(twice.due, UnixTime(DAY + 3 * DAY));
        assert!(!twice.is_learned());
    }

    #[test]
    fn a_miss_brings_the_item_back_within_minutes() {
        let missed = schedule(&[attempt(0, true), attempt(1, true), attempt(4, false)]).unwrap();

        assert_eq!(missed.level, 0);
        assert_eq!(missed.due, UnixTime(4 * DAY + 10 * MINUTE));
        assert!(!missed.is_due(UnixTime(4 * DAY + MINUTE)));
        assert!(missed.is_due(UnixTime(4 * DAY + 11 * MINUTE)));
    }

    #[test]
    fn three_successes_in_a_row_count_as_learned() {
        let learned = schedule(&[attempt(0, false), attempt(0, true), attempt(1, true), attempt(4, true)]).unwrap();

        assert!(learned.is_learned());
    }
}
