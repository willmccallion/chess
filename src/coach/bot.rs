//! The coach's opponent: the engine's scores for every move, played with human-like errors.

use super::judgement::Eval;
use super::rating::Elo;
use crate::search::Score;
use crate::types::Move;
use rand::Rng;

/// Deliberate mistakes give away between these many win-chance points: real errors, but
/// not absurd ones.
const DELIBERATE_MISTAKE_DROP: std::ops::RangeInclusive<f64> = 8.0..=40.0;
/// Beyond this lead winning chances barely change, so extra material counts directly.
const CLEAR_WIN_CP: i32 = 1000;
const CP_PER_UTILITY: f64 = 100.0;
const MATE_UTILITY: f64 = 200.0;

/// How one level plays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LevelSpec {
    /// Roughly the rating of a human the level is an even match for.
    pub elo: Elo,
    /// The search behind each move's score; smaller searches miss more tactics.
    pub nodes_per_move: u64,
    /// Softmax temperature over win-chance points: higher picks worse moves more often.
    pub temperature: f64,
    /// Chance per move of a deliberate mistake.
    pub mistake_chance: f64,
    /// Half-moves for which the level plays from the opening book when it can.
    pub book_plies: usize,
}

/// Interpolated from matches between levels, anchored on Stockfish's UCI_Elo; the Elo is
/// on an engine scale and only roughly a human rating.
const LEVELS: [LevelSpec; 10] = [
    LevelSpec {
        elo: Elo(400),
        nodes_per_move: 610,
        temperature: 8.95,
        mistake_chance: 0.129,
        book_plies: 4,
    },
    LevelSpec {
        elo: Elo(580),
        nodes_per_move: 830,
        temperature: 7.36,
        mistake_chance: 0.105,
        book_plies: 6,
    },
    LevelSpec {
        elo: Elo(760),
        nodes_per_move: 1_100,
        temperature: 6.26,
        mistake_chance: 0.088,
        book_plies: 6,
    },
    LevelSpec {
        elo: Elo(930),
        nodes_per_move: 1_460,
        temperature: 5.38,
        mistake_chance: 0.073,
        book_plies: 6,
    },
    LevelSpec {
        elo: Elo(1110),
        nodes_per_move: 1_940,
        temperature: 4.6,
        mistake_chance: 0.06,
        book_plies: 7,
    },
    LevelSpec {
        elo: Elo(1290),
        nodes_per_move: 2_570,
        temperature: 3.93,
        mistake_chance: 0.049,
        book_plies: 8,
    },
    LevelSpec {
        elo: Elo(1470),
        nodes_per_move: 3_330,
        temperature: 3.36,
        mistake_chance: 0.04,
        book_plies: 9,
    },
    LevelSpec {
        elo: Elo(1640),
        nodes_per_move: 4_450,
        temperature: 2.82,
        mistake_chance: 0.032,
        book_plies: 10,
    },
    LevelSpec {
        elo: Elo(1820),
        nodes_per_move: 6_400,
        temperature: 2.28,
        mistake_chance: 0.024,
        book_plies: 11,
    },
    LevelSpec {
        elo: Elo(2000),
        nodes_per_move: 8_990,
        temperature: 1.82,
        mistake_chance: 0.018,
        book_plies: 12,
    },
];

/// A playing strength of the coach, from 1 (weakest) up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Level(u8);

impl Level {
    pub const WEAKEST: Self = Self(1);
    pub const STRONGEST: Self = Self(LEVELS.len() as u8);

    pub fn new(number: u8) -> Option<Self> {
        (Self::WEAKEST.0..=Self::STRONGEST.0)
            .contains(&number)
            .then_some(Self(number))
    }

    pub const fn number(self) -> u8 {
        self.0
    }

    pub const fn spec(self) -> &'static LevelSpec {
        &LEVELS[self.0 as usize - 1]
    }

    pub fn all() -> impl Iterator<Item = Self> {
        (Self::WEAKEST.0..=Self::STRONGEST.0).map(Self)
    }

    /// The level that is the most even match for a player of `rating`.
    pub fn matching(rating: Elo) -> Self {
        Self::all()
            .min_by_key(|level| (level.spec().elo.0 - rating.0).abs())
            .unwrap_or(Self::WEAKEST)
    }

    pub fn harder(self) -> Self {
        Self::new(self.0 + 1).unwrap_or(self)
    }

    pub fn easier(self) -> Self {
        Self::new(self.0.saturating_sub(1)).unwrap_or(self)
    }
}

/// How much a move's score is worth to the bot: its winning chance, which keeps rising
/// with material beyond a clear win so that won positions get converted, and mates above
/// everything (sooner ones first).
fn utility(score: Score) -> f64 {
    match score {
        Score::MateIn(moves) if moves > 0 => MATE_UTILITY - f64::from(moves),
        Score::MateIn(moves) => -MATE_UTILITY / 2.0 - f64::from(moves),
        Score::Centipawns(cp) => {
            let beyond_clear_win = f64::from(cp.signum() * (cp.abs() - CLEAR_WIN_CP).max(0));
            Eval::Score(score).win_chance().percent() + beyond_clear_win / CP_PER_UTILITY
        }
    }
}

/// Below this much time left the bot searches less, in proportion, so it never loses on time.
const RELAXED_CLOCK_MS: i64 = 60_000;
const MIN_NODES_PER_MOVE: u64 = 200;

impl LevelSpec {
    /// Nodes per move with `remaining_ms` on the bot's clock, if it has one.
    pub fn nodes_for_clock(&self, remaining_ms: Option<i64>) -> u64 {
        match remaining_ms {
            Some(ms) if ms < RELAXED_CLOCK_MS => {
                let share = ms.max(0) as f64 / RELAXED_CLOCK_MS as f64;
                ((self.nodes_per_move as f64 * share) as u64).max(MIN_NODES_PER_MOVE)
            }
            _ => self.nodes_per_move,
        }
    }
}

/// Picks a move from every legal move's score for the side playing it.
pub fn choose_move(scores: &[(Move, Score)], spec: &LevelSpec, rng: &mut impl Rng) -> Option<Move> {
    let chances: Vec<(Move, f64)> = scores
        .iter()
        .map(|&(m, score)| (m, utility(score)))
        .collect();
    let best = chances
        .iter()
        .map(|&(_, chance)| chance)
        .fold(f64::NEG_INFINITY, f64::max);
    if rng.gen_bool(spec.mistake_chance.clamp(0.0, 1.0)) {
        let mistakes: Vec<Move> = chances
            .iter()
            .filter(|&&(_, chance)| DELIBERATE_MISTAKE_DROP.contains(&(best - chance)))
            .map(|&(m, _)| m)
            .collect();
        if !mistakes.is_empty() {
            return Some(mistakes[rng.gen_range(0..mistakes.len())]);
        }
    }
    softmax_pick(&chances, best, spec.temperature, rng)
}

/// Picks one of `moves` with probability in proportion to its weight.
pub fn pick_weighted(moves: &[(Move, u16)], rng: &mut impl Rng) -> Option<Move> {
    let total: u32 = moves.iter().map(|&(_, w)| u32::from(w)).sum();
    if total == 0 {
        return None;
    }
    let mut target = rng.gen_range(0..total);
    for &(m, weight) in moves {
        if target < u32::from(weight) {
            return Some(m);
        }
        target -= u32::from(weight);
    }
    None
}

fn softmax_pick(
    chances: &[(Move, f64)],
    best: f64,
    temperature: f64,
    rng: &mut impl Rng,
) -> Option<Move> {
    let temperature = temperature.max(f64::EPSILON);
    let weights: Vec<f64> = chances
        .iter()
        .map(|&(_, chance)| (-(best - chance) / temperature).exp())
        .collect();
    let total: f64 = weights.iter().sum();
    let mut target = rng.gen_range(0.0..total.max(f64::MIN_POSITIVE));
    for (&(m, _), weight) in chances.iter().zip(&weights) {
        if target < *weight {
            return Some(m);
        }
        target -= weight;
    }
    chances.last().map(|&(m, _)| m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn moves(n: u8) -> Vec<Move> {
        (0..n).map(|i| Move::quiet(i, i + 8)).collect()
    }

    fn picks(scores: &[(Move, Score)], level: Level, games: usize) -> Vec<Move> {
        let mut rng = StdRng::seed_from_u64(7);
        (0..games)
            .filter_map(|_| choose_move(scores, level.spec(), &mut rng))
            .collect()
    }

    #[test]
    fn levels_get_stronger_in_every_respect() {
        for pair in LEVELS.windows(2) {
            assert!(pair[1].elo > pair[0].elo);
            assert!(pair[1].nodes_per_move > pair[0].nodes_per_move);
            assert!(pair[1].temperature < pair[0].temperature);
            assert!(pair[1].mistake_chance < pair[0].mistake_chance);
        }
    }

    #[test]
    fn the_matching_level_is_the_nearest_rating() {
        assert_eq!(Level::matching(Elo(620)), Level::new(2).unwrap());
        assert_eq!(Level::matching(Elo(100)), Level::WEAKEST);
        assert_eq!(Level::matching(Elo(3000)), Level::STRONGEST);
    }

    #[test]
    fn strong_levels_nearly_always_find_the_winning_move() {
        let m = moves(3);
        let scores = [
            (m[0], Score::Centipawns(-50)),
            (m[1], Score::Centipawns(400)),
            (m[2], Score::Centipawns(0)),
        ];

        let chosen = picks(&scores, Level::STRONGEST, 500);

        let found = chosen.iter().filter(|&&c| c == m[1]).count();
        assert!(found >= 490, "{found}");
    }

    #[test]
    fn weak_levels_often_miss_a_free_piece() {
        let m = moves(20);
        let mut scores: Vec<(Move, Score)> =
            m.iter().map(|&mv| (mv, Score::Centipawns(0))).collect();
        scores[0].1 = Score::Centipawns(300);

        let chosen = picks(&scores, Level::WEAKEST, 1000);

        let found = chosen.iter().filter(|&&c| c == m[0]).count();
        assert!((100..700).contains(&found), "{found}");
    }

    #[test]
    fn weighted_picks_follow_the_weights() {
        let m = moves(2);
        let mut rng = StdRng::seed_from_u64(3);

        let picks: Vec<Move> = (0..1000)
            .filter_map(|_| pick_weighted(&[(m[0], 1), (m[1], 9)], &mut rng))
            .collect();

        let heavy = picks.iter().filter(|&&p| p == m[1]).count();
        assert!((850..950).contains(&heavy), "{heavy}");
        assert_eq!(pick_weighted(&[(m[0], 0)], &mut rng), None);
    }

    #[test]
    fn weak_levels_still_take_mate_in_won_positions() {
        let m = moves(20);
        let mut scores: Vec<(Move, Score)> =
            m.iter().map(|&mv| (mv, Score::Centipawns(1500))).collect();
        scores[0].1 = Score::MateIn(1);

        let chosen = picks(&scores, Level::WEAKEST, 1000);

        let mates = chosen.iter().filter(|&&c| c == m[0]).count();
        assert!(mates > 900, "{mates}");
    }

    #[test]
    fn a_short_clock_means_smaller_searches() {
        let spec = Level::STRONGEST.spec();

        assert_eq!(spec.nodes_for_clock(None), spec.nodes_per_move);
        assert_eq!(spec.nodes_for_clock(Some(300_000)), spec.nodes_per_move);
        assert_eq!(spec.nodes_for_clock(Some(30_000)), spec.nodes_per_move / 2);
        assert_eq!(spec.nodes_for_clock(Some(0)), MIN_NODES_PER_MOVE);
    }

    #[test]
    fn no_moves_means_no_choice() {
        assert_eq!(picks(&[], Level::WEAKEST, 1), Vec::new());
    }
}
