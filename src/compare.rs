//! Side-by-side analysis of positions by this engine and a reference engine (e.g. Stockfish).

use crate::uci_client::{Analysis, UciEngine, UciScore};

pub struct CompareConfig {
    pub engine_path: String,
    pub reference_path: String,
    pub engine_options: Vec<(String, String)>,
    pub reference_options: Vec<(String, String)>,
    pub fens: Vec<String>,
    pub engine_go: String,
    pub reference_go: String,
}

struct Comparison {
    engine: Analysis,
    reference: Analysis,
    engine_static_cp: Option<i32>,
    reference_static_cp: Option<i32>,
}

#[derive(Debug, Default, PartialEq)]
struct Summary {
    positions: usize,
    best_move_matches: usize,
    /// Positions where both engines gave a centipawn score.
    scored_pairs: usize,
    mean_abs_score_diff: f64,
    mean_signed_score_diff: f64,
    same_sign_scores: usize,
    static_pairs: usize,
    mean_abs_static_diff: f64,
}

fn centipawn_pair(a: Option<UciScore>, b: Option<UciScore>) -> Option<(i32, i32)> {
    match (a?, b?) {
        (UciScore::Centipawns(x), UciScore::Centipawns(y)) => Some((x, y)),
        _ => None,
    }
}

fn summarize(comparisons: &[Comparison]) -> Summary {
    let best_move_matches = comparisons
        .iter()
        .filter(|c| c.engine.best_move.is_some() && c.engine.best_move == c.reference.best_move)
        .count();

    let score_pairs: Vec<(i32, i32)> = comparisons
        .iter()
        .filter_map(|c| centipawn_pair(c.engine.score, c.reference.score))
        .collect();
    let static_pairs: Vec<(i32, i32)> = comparisons
        .iter()
        .filter_map(|c| Some((c.engine_static_cp?, c.reference_static_cp?)))
        .collect();

    let mean = |values: &mut dyn Iterator<Item = f64>, n: usize| {
        if n == 0 {
            0.0
        } else {
            values.sum::<f64>() / n as f64
        }
    };

    Summary {
        positions: comparisons.len(),
        best_move_matches,
        scored_pairs: score_pairs.len(),
        mean_abs_score_diff: mean(
            &mut score_pairs.iter().map(|(e, r)| f64::from((e - r).abs())),
            score_pairs.len(),
        ),
        mean_signed_score_diff: mean(
            &mut score_pairs.iter().map(|(e, r)| f64::from(e - r)),
            score_pairs.len(),
        ),
        same_sign_scores: score_pairs
            .iter()
            .filter(|(e, r)| e.signum() == r.signum())
            .count(),
        static_pairs: static_pairs.len(),
        mean_abs_static_diff: mean(
            &mut static_pairs.iter().map(|(e, r)| f64::from((e - r).abs())),
            static_pairs.len(),
        ),
    }
}

fn format_score(score: Option<UciScore>) -> String {
    match score {
        Some(UciScore::Centipawns(cp)) => format!("{cp:+}"),
        Some(UciScore::Mate(moves)) => format!("M{moves}"),
        None => "-".to_string(),
    }
}

fn format_static(cp: Option<i32>) -> String {
    cp.map_or_else(|| "-".to_string(), |v| format!("{v:+}"))
}

fn format_diff(engine: Option<UciScore>, reference: Option<UciScore>) -> String {
    centipawn_pair(engine, reference)
        .map_or_else(|| "-".to_string(), |(e, r)| format!("{:+}", e - r))
}

fn spawn(path: &str, options: &[(String, String)]) -> Result<UciEngine, String> {
    let mut engine = UciEngine::spawn(path)?;
    engine.init_uci()?;
    engine.set_options(options)?;
    engine.is_ready()?;
    Ok(engine)
}

fn print_row(index: usize, fen: &str, c: &Comparison) {
    let best = |a: &Analysis| a.best_move.clone().unwrap_or_else(|| "-".to_string());
    let marker = if c.engine.best_move.is_some() && c.engine.best_move == c.reference.best_move {
        "="
    } else {
        " "
    };
    println!(
        "{:>3}  {:>7} {:>6} d{:<3} | {:>7} {:>6} d{:<3} | {:>6} {} | static {:>6} vs {:>6}",
        index + 1,
        format_score(c.engine.score),
        best(&c.engine),
        c.engine.depth,
        format_score(c.reference.score),
        best(&c.reference),
        c.reference.depth,
        format_diff(c.engine.score, c.reference.score),
        marker,
        format_static(c.engine_static_cp),
        format_static(c.reference_static_cp),
    );
    println!("     {fen}");
}

fn print_summary(s: &Summary, engine_name: &str, reference_name: &str) {
    let pct = |n: usize, d: usize| {
        if d == 0 {
            0.0
        } else {
            n as f64 * 100.0 / d as f64
        }
    };
    println!();
    println!("Summary ({engine_name} vs {reference_name}, scores from side to move, cp)");
    println!(
        "  Best move agreement : {}/{} ({:.1}%)",
        s.best_move_matches,
        s.positions,
        pct(s.best_move_matches, s.positions)
    );
    if s.scored_pairs > 0 {
        println!(
            "  Search score        : mean |diff| {:.1}, mean diff {:+.1}, same sign {}/{} ({:.1}%)",
            s.mean_abs_score_diff,
            s.mean_signed_score_diff,
            s.same_sign_scores,
            s.scored_pairs,
            pct(s.same_sign_scores, s.scored_pairs)
        );
    }
    if s.static_pairs > 0 {
        println!(
            "  Static eval (White) : mean |diff| {:.1} over {} positions",
            s.mean_abs_static_diff, s.static_pairs
        );
    }
}

fn display_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub fn run_compare(config: &CompareConfig) -> Result<(), String> {
    let mut engine = spawn(&config.engine_path, &config.engine_options)?;
    let mut reference = spawn(&config.reference_path, &config.reference_options)?;
    let engine_name = display_name(&config.engine_path);
    let reference_name = display_name(&config.reference_path);

    println!(
        "  #  {:>22} | {:>22} |   diff",
        format!("{engine_name} (go {})", config.engine_go),
        format!("{reference_name} (go {})", config.reference_go)
    );

    let mut comparisons = Vec::with_capacity(config.fens.len());
    for (index, fen) in config.fens.iter().enumerate() {
        let comparison = Comparison {
            engine_static_cp: engine.static_eval_white_cp(fen)?,
            reference_static_cp: reference.static_eval_white_cp(fen)?,
            engine: engine.analyse(fen, &config.engine_go)?,
            reference: reference.analyse(fen, &config.reference_go)?,
        };
        print_row(index, fen, &comparison);
        comparisons.push(comparison);
    }

    print_summary(&summarize(&comparisons), engine_name, reference_name);
    engine.quit();
    reference.quit();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analysis(score: Option<UciScore>, best: &str) -> Analysis {
        Analysis {
            score,
            depth: 10,
            best_move: Some(best.to_string()),
        }
    }

    fn comparison(engine: Analysis, reference: Analysis) -> Comparison {
        Comparison {
            engine,
            reference,
            engine_static_cp: None,
            reference_static_cp: None,
        }
    }

    #[test]
    fn summary_counts_matching_moves_and_score_gaps() {
        let comparisons = [
            comparison(
                analysis(Some(UciScore::Centipawns(50)), "e2e4"),
                analysis(Some(UciScore::Centipawns(30)), "e2e4"),
            ),
            comparison(
                analysis(Some(UciScore::Centipawns(-10)), "d2d4"),
                analysis(Some(UciScore::Centipawns(20)), "c2c4"),
            ),
        ];

        let summary = summarize(&comparisons);

        assert_eq!(summary.positions, 2);
        assert_eq!(summary.best_move_matches, 1);
        assert_eq!(summary.scored_pairs, 2);
        assert!((summary.mean_abs_score_diff - 25.0).abs() < 1e-9);
        assert!((summary.mean_signed_score_diff - (-5.0)).abs() < 1e-9);
        assert_eq!(summary.same_sign_scores, 1);
    }

    #[test]
    fn mate_scores_are_excluded_from_centipawn_statistics() {
        let comparisons = [comparison(
            analysis(Some(UciScore::Mate(3)), "a1a8"),
            analysis(Some(UciScore::Centipawns(900)), "a1a8"),
        )];

        let summary = summarize(&comparisons);

        assert_eq!(summary.best_move_matches, 1);
        assert_eq!(summary.scored_pairs, 0);
    }
}
