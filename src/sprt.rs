//! Pentanomial game-pair statistics and a GSPRT stopping rule in logistic Elo.

const Z_95: f64 = 1.96;

/// Outcome of a game pair from engine 1's perspective, in half-points (0..=4).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PairScore(u8);

impl PairScore {
    /// Combines two per-game scores (0.0, 0.5 or 1.0) into a pair score.
    pub fn from_game_points(first: f64, second: f64) -> Self {
        let half_points = ((first + second) * 2.0).round() as u8;
        Self(half_points.min(4))
    }
}

#[derive(Clone, Debug, Default)]
pub struct Pentanomial {
    counts: [u64; 5],
}

impl Pentanomial {
    pub fn add(&mut self, pair: PairScore) {
        self.counts[pair.0 as usize] += 1;
    }

    pub fn counts(&self) -> [u64; 5] {
        self.counts
    }

    pub fn pairs(&self) -> u64 {
        self.counts.iter().sum()
    }

    /// Mean per-game score and per-pair variance of the pair-averaged score.
    fn mean_and_variance(&self) -> Option<(f64, f64)> {
        let n = self.pairs() as f64;
        if n == 0.0 {
            return None;
        }
        let pair_score = |i: usize| i as f64 / 4.0;
        let mean = (0..5)
            .map(|i| self.counts[i] as f64 * pair_score(i))
            .sum::<f64>()
            / n;
        let variance = (0..5)
            .map(|i| self.counts[i] as f64 * (pair_score(i) - mean).powi(2))
            .sum::<f64>()
            / n;
        Some((mean, variance))
    }

    /// Logistic Elo estimate with a 95% confidence half-width.
    pub fn elo(&self) -> Option<(f64, f64)> {
        let (mean, variance) = self.mean_and_variance()?;
        if mean <= 0.0 || mean >= 1.0 {
            return None;
        }
        let standard_error = (variance / self.pairs() as f64).sqrt();
        let lower = (mean - Z_95 * standard_error).clamp(1e-6, 1.0 - 1e-6);
        let upper = (mean + Z_95 * standard_error).clamp(1e-6, 1.0 - 1e-6);
        let error = (score_to_elo(upper) - score_to_elo(lower)) / 2.0;
        Some((score_to_elo(mean), error))
    }
}

fn score_to_elo(score: f64) -> f64 {
    -400.0 * (1.0 / score - 1.0).log10()
}

fn elo_to_score(elo: f64) -> f64 {
    1.0 / (1.0 + 10f64.powf(-elo / 400.0))
}

#[derive(Copy, Clone, Debug)]
pub struct SprtConfig {
    pub elo0: f64,
    pub elo1: f64,
    pub alpha: f64,
    pub beta: f64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SprtDecision {
    Continue,
    AcceptH0,
    AcceptH1,
}

impl SprtConfig {
    /// Parses `elo0,elo1` or `elo0,elo1,alpha,beta` (alpha and beta default to 0.05).
    pub fn parse(s: &str) -> Result<Self, String> {
        let fields: Vec<f64> = s
            .split(',')
            .map(|f| f.trim().parse::<f64>())
            .collect::<Result<_, _>>()
            .map_err(|e| format!("invalid --sprt '{s}': {e}"))?;
        let (elo0, elo1, alpha, beta) = match fields.as_slice() {
            [elo0, elo1] => (*elo0, *elo1, 0.05, 0.05),
            [elo0, elo1, alpha, beta] => (*elo0, *elo1, *alpha, *beta),
            _ => {
                return Err(format!(
                    "invalid --sprt '{s}': expected elo0,elo1[,alpha,beta]"
                ));
            }
        };
        if elo1 <= elo0 {
            return Err(format!("invalid --sprt '{s}': elo1 must exceed elo0"));
        }
        let valid_probability = |p: f64| p > 0.0 && p < 0.5;
        if !valid_probability(alpha) || !valid_probability(beta) {
            return Err(format!(
                "invalid --sprt '{s}': alpha and beta must be in (0, 0.5)"
            ));
        }
        Ok(Self {
            elo0,
            elo1,
            alpha,
            beta,
        })
    }

    pub fn lower_bound(&self) -> f64 {
        (self.beta / (1.0 - self.alpha)).ln()
    }

    pub fn upper_bound(&self) -> f64 {
        ((1.0 - self.beta) / self.alpha).ln()
    }

    /// Generalized SPRT log-likelihood ratio under the normal approximation.
    pub fn llr(&self, penta: &Pentanomial) -> f64 {
        let Some((mean, variance)) = penta.mean_and_variance() else {
            return 0.0;
        };
        if variance <= 0.0 {
            return 0.0;
        }
        let s0 = elo_to_score(self.elo0);
        let s1 = elo_to_score(self.elo1);
        penta.pairs() as f64 * (s1 - s0) * (2.0 * mean - s0 - s1) / (2.0 * variance)
    }

    pub fn decide(&self, llr: f64) -> SprtDecision {
        if llr >= self.upper_bound() {
            SprtDecision::AcceptH1
        } else if llr <= self.lower_bound() {
            SprtDecision::AcceptH0
        } else {
            SprtDecision::Continue
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn penta_from(counts: [u64; 5]) -> Pentanomial {
        Pentanomial { counts }
    }

    #[test]
    fn pair_score_combines_game_points() {
        assert_eq!(PairScore::from_game_points(1.0, 0.5), PairScore(3));
        assert_eq!(PairScore::from_game_points(0.0, 0.0), PairScore(0));
        assert_eq!(PairScore::from_game_points(1.0, 1.0), PairScore(4));
    }

    #[test]
    fn parses_short_and_full_sprt_specs() {
        let short = SprtConfig::parse("0,5").unwrap();
        assert_eq!((short.elo0, short.elo1, short.alpha), (0.0, 5.0, 0.05));
        let full = SprtConfig::parse("-5, 0, 0.1, 0.1").unwrap();
        assert_eq!((full.elo0, full.elo1, full.beta), (-5.0, 0.0, 0.1));
        assert!(SprtConfig::parse("5,0").is_err());
        assert!(SprtConfig::parse("0").is_err());
    }

    #[test]
    fn llr_accepts_h1_for_clearly_stronger_engine() {
        let sprt = SprtConfig::parse("0,5").unwrap();
        let penta = penta_from([20, 150, 500, 250, 80]);
        assert_eq!(sprt.decide(sprt.llr(&penta)), SprtDecision::AcceptH1);
    }

    #[test]
    fn llr_accepts_h0_for_clearly_weaker_engine() {
        let sprt = SprtConfig::parse("0,5").unwrap();
        let penta = penta_from([80, 250, 500, 150, 20]);
        assert_eq!(sprt.decide(sprt.llr(&penta)), SprtDecision::AcceptH0);
    }

    #[test]
    fn llr_continues_with_little_data() {
        let sprt = SprtConfig::parse("0,5").unwrap();
        let penta = penta_from([1, 2, 5, 2, 1]);
        assert_eq!(sprt.decide(sprt.llr(&penta)), SprtDecision::Continue);
    }

    #[test]
    fn elo_is_zero_for_symmetric_results() {
        let (elo, error) = penta_from([10, 40, 100, 40, 10]).elo().unwrap();
        assert!(elo.abs() < 1e-9);
        assert!(error > 0.0);
    }
}
