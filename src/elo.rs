//! Elo differences from match scores, on the logistic scale.

use crate::game_runner::Wdl;

/// The two-sided 95% quantile of the normal distribution.
pub(crate) const Z_95: f64 = 1.96;

/// The Elo difference at which the stronger side scores `score` (0 to 1).
pub(crate) fn score_to_elo(score: f64) -> f64 {
    -400.0 * (1.0 / score - 1.0).log10()
}

/// The score (0 to 1) the stronger side expects at an Elo difference of `elo`.
pub(crate) fn elo_to_score(elo: f64) -> f64 {
    1.0 / (1.0 + 10f64.powf(-elo / 400.0))
}

/// The Elo difference `wdl` implies, or `None` without games or after a
/// clean sweep, where it is infinite.
pub(crate) fn elo_of(wdl: Wdl) -> Option<f64> {
    let score = score_of(wdl)?;
    (score > 0.0 && score < 1.0).then(|| score_to_elo(score))
}

/// The half-width of a 95% confidence interval around [`elo_of`], counting
/// games as independent; `None` with fewer than two games or a clean sweep.
pub(crate) fn elo_error_95(wdl: Wdl) -> Option<f64> {
    let games = wdl.games() as f64;
    let score = score_of(wdl).filter(|&s| games >= 2.0 && s > 0.0 && s < 1.0)?;
    let standard_error = (score * (1.0 - score) / games).sqrt();
    let lower = (score - Z_95 * standard_error).max(0.001);
    let upper = (score + Z_95 * standard_error).min(0.999);
    Some((score_to_elo(upper) - score_to_elo(lower)) / 2.0)
}

fn score_of(wdl: Wdl) -> Option<f64> {
    (wdl.games() > 0).then(|| wdl.points() / wdl.games() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn wdl(wins: usize, losses: usize, draws: usize) -> Wdl {
        Wdl {
            wins,
            losses,
            draws,
        }
    }

    #[test]
    fn elo_is_undefined_without_games_or_with_a_clean_sweep() {
        assert_eq!(elo_of(wdl(0, 0, 0)), None);
        assert_eq!(elo_of(wdl(5, 0, 0)), None);
        assert_eq!(elo_of(wdl(0, 5, 0)), None);
    }

    #[test]
    fn elo_follows_the_logistic_curve() {
        let even = elo_of(wdl(3, 3, 4)).unwrap();
        let three_quarters = elo_of(wdl(3, 1, 0)).unwrap();

        assert!(even.abs() < 1e-9, "{even}");
        assert!(
            (three_quarters - 400.0 * 3f64.log10()).abs() < 1e-9,
            "{three_quarters}"
        );
    }

    #[test]
    fn the_error_bar_needs_two_games_and_a_mixed_score() {
        assert_eq!(elo_error_95(wdl(1, 0, 0)), None);
        assert_eq!(elo_error_95(wdl(0, 0, 0)), None);
        assert_eq!(elo_error_95(wdl(4, 0, 0)), None);
        assert!(elo_error_95(wdl(10, 10, 0)).is_some_and(|e| e.is_finite() && e > 0.0));
    }

    #[test]
    fn score_and_elo_convert_into_each_other() {
        for elo in [-300.0, 0.0, 120.0] {
            assert!(
                (score_to_elo(elo_to_score(elo)) - elo).abs() < 1e-9,
                "{elo}"
            );
        }
    }
}
