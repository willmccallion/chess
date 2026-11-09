use crate::types::Centipawns;
use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub};

/// The search measures scores in units of 3/4 centipawn: its margins were set for an
/// evaluation about 4/3 the size of Stockfish 18's centipawns.
const UNITS_PER_CP_NUM: i32 = 4;
const UNITS_PER_CP_DEN: i32 = 3;

/// A score from the search's point of view: an evaluation in its own units,
/// or a mate, counted in plies from the root, near `±MATE`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct SearchScore(i32);

impl SearchScore {
    pub const ZERO: Self = Self(0);
    pub const DRAW: Self = Self::ZERO;
    /// The smallest difference between two scores, which a null window spans.
    pub const UNIT: Self = Self(1);
    /// Mate on the board, at the root.
    pub const MATE: Self = Self(30_000);
    /// Past this, a score is a mate.
    pub const MATE_THRESHOLD: Self = Self(Self::MATE.0 - 512);
    /// Past every score a search can return, mates included: the widest window.
    pub const BOUND: Self = Self(Self::MATE.0 + 1);

    pub const fn new(units: i32) -> Self {
        Self(units)
    }

    pub const fn units(self) -> i32 {
        self.0
    }

    /// The side to move is checkmated at `ply`.
    pub const fn mated_at(ply: usize) -> Self {
        Self(-Self::MATE.0 + ply as i32)
    }

    /// The side to move gives mate at `ply`.
    pub const fn mate_at(ply: usize) -> Self {
        Self(Self::MATE.0 - ply as i32)
    }

    pub const fn from_centipawns(cp: Centipawns) -> Self {
        Self(cp.get() * UNITS_PER_CP_NUM / UNITS_PER_CP_DEN)
    }

    pub const fn to_centipawns(self) -> Centipawns {
        Centipawns::new(self.0 * UNITS_PER_CP_DEN / UNITS_PER_CP_NUM)
    }

    /// `cp` taken as search units as it is, where a margin was tuned that way.
    pub const fn unscaled(cp: Centipawns) -> Self {
        Self(cp.get())
    }

    /// These units taken as centipawns as they are; the inverse of [`Self::unscaled`].
    pub const fn as_unscaled_centipawns(self) -> Centipawns {
        Centipawns::new(self.0)
    }

    #[must_use]
    pub const fn abs(self) -> Self {
        Self(self.0.abs())
    }

    /// Halfway between `self` and `other`, rounded toward negative infinity.
    #[must_use]
    pub const fn midpoint(self, other: Self) -> Self {
        Self(i32::midpoint(self.0, other.0))
    }

    /// This score as the TT keeps it at `ply`: a mate counted from the node.
    pub const fn to_tt(self, ply: usize) -> TtScore {
        TtScore(if self.0 > Self::MATE_THRESHOLD.0 {
            self.0 + ply as i32
        } else if self.0 < -Self::MATE_THRESHOLD.0 {
            self.0 - ply as i32
        } else {
            self.0
        })
    }
}

impl Add for SearchScore {
    type Output = Self;

    #[inline(always)]
    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

impl Sub for SearchScore {
    type Output = Self;

    #[inline(always)]
    fn sub(self, other: Self) -> Self {
        Self(self.0 - other.0)
    }
}

impl AddAssign for SearchScore {
    #[inline(always)]
    fn add_assign(&mut self, other: Self) {
        self.0 += other.0;
    }
}

impl Neg for SearchScore {
    type Output = Self;

    #[inline(always)]
    fn neg(self) -> Self {
        Self(-self.0)
    }
}

impl Mul<i32> for SearchScore {
    type Output = Self;

    #[inline(always)]
    fn mul(self, factor: i32) -> Self {
        Self(self.0 * factor)
    }
}

/// A fraction of the score, rounded toward zero.
impl Div<i32> for SearchScore {
    type Output = Self;

    #[inline(always)]
    fn div(self, divisor: i32) -> Self {
        Self(self.0 / divisor)
    }
}

/// How many times `divisor` fits in the score, rounded toward zero.
impl Div for SearchScore {
    type Output = i32;

    #[inline(always)]
    fn div(self, divisor: Self) -> i32 {
        self.0 / divisor.0
    }
}

/// A score as the TT stores it, with a mate counted from the node instead of
/// the root; [`TtScore::at_ply`] turns it back into a search score.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TtScore(i32);

impl TtScore {
    /// The score read back at `ply`.
    pub const fn at_ply(self, ply: usize) -> SearchScore {
        SearchScore(if self.0 > SearchScore::MATE_THRESHOLD.0 {
            self.0 - ply as i32
        } else if self.0 < -SearchScore::MATE_THRESHOLD.0 {
            self.0 + ply as i32
        } else {
            self.0
        })
    }

    /// The 16 bits the table packs, which every search score fits in.
    pub(crate) const fn to_bits(self) -> i16 {
        self.0 as i16
    }

    pub(crate) const fn from_bits(bits: i16) -> Self {
        Self(bits as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tt_score_reads_back_as_stored_at_the_same_ply() {
        let threshold = SearchScore::MATE_THRESHOLD;
        let scores = [
            SearchScore::DRAW,
            SearchScore::new(250),
            SearchScore::new(-250),
            threshold,
            -threshold,
            threshold + SearchScore::UNIT,
            -threshold - SearchScore::UNIT,
            SearchScore::mate_at(1),
            SearchScore::mated_at(1),
        ];
        for score in scores {
            for ply in [0, 7, 120] {
                assert_eq!(score.to_tt(ply).at_ply(ply), score, "{score:?} at {ply}");
            }
        }
    }

    #[test]
    fn a_mate_keeps_its_distance_from_the_node_whatever_ply_reads_it() {
        let stored =
            [SearchScore::mate_at(3 + 2), SearchScore::mated_at(3 + 2)].map(|s| s.to_tt(3));

        let read_at_ply_10 = stored.map(|s| s.at_ply(10));

        assert_eq!(
            read_at_ply_10,
            [SearchScore::mate_at(10 + 2), SearchScore::mated_at(10 + 2)]
        );
    }

    #[test]
    fn scores_short_of_mate_pass_through_the_tt_unchanged() {
        let threshold = SearchScore::MATE_THRESHOLD;
        for score in [threshold, -threshold, SearchScore::DRAW] {
            assert_eq!(score.to_tt(50), TtScore(score.units()));
            assert_eq!(TtScore(score.units()).at_ply(50), score);
        }
    }

    #[test]
    fn evaluations_scale_to_search_units_and_back() {
        let cp = Centipawns::new(300);

        let score = SearchScore::from_centipawns(cp);

        assert_eq!(score, SearchScore::new(400));
        assert_eq!(score.to_centipawns(), cp);
    }
}
