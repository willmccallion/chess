//! Elo ratings for you, the coach's levels and puzzles.

/// A rating on the Elo scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Elo(pub i32);

impl Elo {
    /// Where a new player starts.
    pub const START: Self = Self(600);
    const FLOOR: Self = Self(100);
}

/// Games or puzzles played before a rating settles and moves more slowly.
const PROVISIONAL_GAMES: usize = 20;
const PROVISIONAL_K: f64 = 40.0;
const SETTLED_K: f64 = 20.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    Win,
    Draw,
    Loss,
}

impl Outcome {
    pub const fn points(self) -> f64 {
        match self {
            Self::Win => 1.0,
            Self::Draw => 0.5,
            Self::Loss => 0.0,
        }
    }
}

/// The score `player` is expected to make against `opponent`, from 0 to 1.
pub fn expected_score(player: Elo, opponent: Elo) -> f64 {
    1.0 / (1.0 + 10f64.powf(f64::from(opponent.0 - player.0) / 400.0))
}

/// `player`'s rating after `outcome` against `opponent`, having played `games_before` before.
pub fn updated(player: Elo, opponent: Elo, outcome: Outcome, games_before: usize) -> Elo {
    let k = if games_before < PROVISIONAL_GAMES {
        PROVISIONAL_K
    } else {
        SETTLED_K
    };
    let change = k * (outcome.points() - expected_score(player, opponent));
    Elo((f64::from(player.0) + change).round() as i32).max(Elo::FLOOR)
}

/// A rating after a run of results, each against an opponent rating, from [`Elo::START`].
pub fn rating_after(results: impl IntoIterator<Item = (Elo, Outcome)>) -> Elo {
    results
        .into_iter()
        .enumerate()
        .fold(Elo::START, |rating, (played, (opponent, outcome))| {
            updated(rating, opponent, outcome, played)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_players_expect_half_a_point() {
        assert!((expected_score(Elo(800), Elo(800)) - 0.5).abs() < 1e-9);
        assert!(expected_score(Elo(1200), Elo(800)) > 0.9);
    }

    #[test]
    fn beating_a_stronger_opponent_gains_more_than_beating_a_weaker_one() {
        let upset = updated(Elo(600), Elo(900), Outcome::Win, 30);
        let expected = updated(Elo(600), Elo(300), Outcome::Win, 30);

        assert!(upset.0 - 600 > expected.0 - 600);
        assert!(expected > Elo(600));
    }

    #[test]
    fn new_players_move_faster() {
        let new = updated(Elo(600), Elo(600), Outcome::Win, 0);
        let settled = updated(Elo(600), Elo(600), Outcome::Win, 50);

        assert_eq!(new, Elo(620));
        assert_eq!(settled, Elo(610));
    }

    #[test]
    fn ratings_never_fall_below_the_floor() {
        let losses = std::iter::repeat_n((Elo(100), Outcome::Loss), 200);

        assert_eq!(rating_after(losses), Elo::FLOOR);
    }
}
