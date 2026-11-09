use crate::game_runner::{
    GameClock, GameConfig, GameEndReason, GameResult, StartPosition, play_game,
};
use crate::sprt::{PairScore, Pentanomial, SprtConfig, SprtDecision};
use crate::types::START_FEN;
use crate::uci_client::UciEngine;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

pub struct TournamentConfig {
    pub engine1_path: String,
    pub engine2_path: String,
    pub e1_options: Vec<(String, String)>,
    pub e2_options: Vec<(String, String)>,
    pub e1_name: String,
    pub e2_name: String,
    pub e1_clock: GameClock,
    pub e2_clock: GameClock,
    pub rounds: usize,
    pub concurrency: usize,
    /// Where every game starts when there are no openings.
    pub start: StartPosition,
    /// Start positions; game pair `p` plays `openings[p % len]` with colors swapped.
    pub openings: Vec<StartPosition>,
    pub sprt: Option<SprtConfig>,
}

/// The name a tournament shows for an engine: `name` if given, else the binary's file name.
pub fn engine_name(name: Option<String>, path: &str) -> String {
    name.unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path).to_string())
}

impl TournamentConfig {
    fn start_for_game(&self, game_idx: usize) -> &StartPosition {
        if self.openings.is_empty() {
            return &self.start;
        }
        let pair_idx = game_idx / 2;
        &self.openings[pair_idx % self.openings.len()]
    }
}

struct Score {
    e1_wins: usize,
    e2_wins: usize,
    draws: usize,
    completed: usize,

    // As white / as black (from e1's perspective)
    e1_wins_as_white: usize,
    e1_wins_as_black: usize,
    e2_wins_as_white: usize,
    e2_wins_as_black: usize,
    draws_e1_white: usize,
    draws_e1_black: usize,

    // Termination reasons
    checkmates: usize,
    stalemates: usize,
    repetitions: usize,
    fifty_move: usize,
    insufficient_material: usize,
    flag_falls: usize,
    move_limits: usize,
    illegal_moves: usize,
    crashes: usize,

    // Game length tracking
    total_plies: usize,
    shortest_game: usize,
    longest_game: usize,

    // Game-pair statistics (pair = same opening, colors swapped)
    penta: Pentanomial,
    unpaired_e1_points: HashMap<usize, f64>,
    sprt_decision: Option<SprtDecision>,
}

impl Score {
    fn new() -> Self {
        Self {
            e1_wins: 0,
            e2_wins: 0,
            draws: 0,
            completed: 0,
            e1_wins_as_white: 0,
            e1_wins_as_black: 0,
            e2_wins_as_white: 0,
            e2_wins_as_black: 0,
            draws_e1_white: 0,
            draws_e1_black: 0,
            checkmates: 0,
            stalemates: 0,
            repetitions: 0,
            fifty_move: 0,
            insufficient_material: 0,
            flag_falls: 0,
            move_limits: 0,
            illegal_moves: 0,
            crashes: 0,
            total_plies: 0,
            shortest_game: usize::MAX,
            longest_game: 0,
            penta: Pentanomial::default(),
            unpaired_e1_points: HashMap::new(),
            sprt_decision: None,
        }
    }

    /// Records engine 1's points for a game; returns true when this completes a pair.
    fn record_pair_game(&mut self, game_idx: usize, e1_points: f64) -> bool {
        let pair_idx = game_idx / 2;
        match self.unpaired_e1_points.remove(&pair_idx) {
            Some(partner_points) => {
                self.penta
                    .add(PairScore::from_game_points(partner_points, e1_points));
                true
            }
            None => {
                self.unpaired_e1_points.insert(pair_idx, e1_points);
                false
            }
        }
    }
}

pub fn parse_option(s: &str) -> Result<(String, String), String> {
    let (name, value) = s
        .split_once('=')
        .ok_or_else(|| format!("Invalid option '{}', expected Name=Value", s))?;
    Ok((name.trim().to_string(), value.trim().to_string()))
}

fn elo_diff(wins: usize, losses: usize, draws: usize) -> Option<f64> {
    let total = (wins + losses + draws) as f64;
    if total == 0.0 {
        return None;
    }
    let score = (wins as f64 + draws as f64 * 0.5) / total;
    if score <= 0.0 || score >= 1.0 {
        return None;
    }
    Some(-400.0 * (1.0 / score - 1.0).log10())
}

fn elo_error_95(wins: usize, losses: usize, draws: usize) -> Option<f64> {
    let total = (wins + losses + draws) as f64;
    if total < 2.0 {
        return None;
    }
    let score = (wins as f64 + draws as f64 * 0.5) / total;
    if score <= 0.0 || score >= 1.0 {
        return None;
    }
    let se = (score * (1.0 - score) / total).sqrt();
    let lo = (score - 1.96 * se).max(0.001);
    let hi = (score + 1.96 * se).min(0.999);
    let elo_lo = -400.0 * (1.0 / lo - 1.0).log10();
    let elo_hi = -400.0 * (1.0 / hi - 1.0).log10();
    Some((elo_hi - elo_lo) / 2.0)
}

fn format_elo(wins: usize, losses: usize, draws: usize) -> String {
    match elo_diff(wins, losses, draws) {
        Some(e) => format!("{:+.0}", e),
        None if wins > losses => "+inf".to_string(),
        None if losses > wins => "-inf".to_string(),
        _ => "0".to_string(),
    }
}

fn format_pct(n: usize, total: usize) -> String {
    if total == 0 {
        "  -  ".to_string()
    } else {
        format!("{:5.1}%", n as f64 / total as f64 * 100.0)
    }
}

/// Starts and configures an engine, restricted to the CPU list `cpus` if given.
pub(crate) fn spawn_and_init_pinned(
    path: &str,
    options: &[(String, String)],
    cpus: Option<&str>,
) -> Result<UciEngine, String> {
    let mut engine = UciEngine::spawn_pinned(path, cpus)?;
    engine.init_uci()?;
    if !options
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("Threads"))
    {
        engine.set_options(&[("Threads".to_string(), "1".to_string())])?;
    }
    engine.set_options(options)?;
    engine.is_ready()?;
    Ok(engine)
}

fn print_summary(s: &Score, e1_name: &str, e2_name: &str, elapsed: f64) {
    let total = s.completed;
    let e1_score = s.e1_wins as f64 + s.draws as f64 * 0.5;
    let e2_score = s.e2_wins as f64 + s.draws as f64 * 0.5;
    let elo = format_elo(s.e1_wins, s.e2_wins, s.draws);
    let err = match s.penta.elo() {
        Some((_, pair_error)) => Some(pair_error),
        None => elo_error_95(s.e1_wins, s.e2_wins, s.draws),
    };
    let draw_rate = if total > 0 {
        s.draws as f64 / total as f64 * 100.0
    } else {
        0.0
    };
    let avg_len = if total > 0 {
        s.total_plies as f64 / total as f64
    } else {
        0.0
    };

    // Width for engine name columns
    let w = e1_name.len().max(e2_name.len()).max(6);

    println!();
    println!("=================================================================");
    println!("  TOURNAMENT RESULTS");
    println!("=================================================================");
    println!();

    // Score line
    let elo_str = match err {
        Some(e) => format!("{} +/- {:.0} Elo", elo, e),
        None => format!("{} Elo", elo),
    };
    println!("  Score: {:.1} - {:.1}  [{elo_str}]", e1_score, e2_score);
    println!("  Games: {}    Draw rate: {:.1}%", total, draw_rate);
    if s.penta.pairs() > 0 {
        let [ll, ld, even, wd, ww] = s.penta.counts();
        println!(
            "  Pairs: {}    Pentanomial [LL LD DD/WL WD WW]: [{} {} {} {} {}]",
            s.penta.pairs(),
            ll,
            ld,
            even,
            wd,
            ww
        );
    }
    if let Some(decision) = s.sprt_decision {
        let verdict = match decision {
            SprtDecision::AcceptH1 => "H1 accepted (engine1 is stronger)",
            SprtDecision::AcceptH0 => "H0 accepted (no gain shown)",
            SprtDecision::Continue => "inconclusive (game limit reached)",
        };
        println!("  SPRT:  {verdict}");
    }
    if elapsed > 0.0 {
        let gps = total as f64 / elapsed;
        println!("  Time:  {:.1}s ({:.1} games/sec)", elapsed, gps);
    }
    println!();

    // Head-to-head table
    println!(
        "  +-{0:-<w$}-+------+------+------+-------+-------+",
        "",
        w = w
    );
    println!(
        "  | {0:<w$} | {1:>4} | {2:>4} | {3:>4} | {4:>5} | {5:>5} |",
        "Engine",
        "W",
        "L",
        "D",
        "Score",
        "Pts",
        w = w
    );
    println!(
        "  +-{0:-<w$}-+------+------+------+-------+-------+",
        "",
        w = w
    );
    println!(
        "  | {name:<w$} | {wins:>4} | {losses:>4} | {draws:>4} | {pct} | {pts:>5.1} |",
        name = e1_name,
        wins = s.e1_wins,
        losses = s.e2_wins,
        draws = s.draws,
        pct = format_pct(s.e1_wins * 2 + s.draws, total * 2),
        pts = e1_score,
        w = w,
    );
    println!(
        "  | {name:<w$} | {wins:>4} | {losses:>4} | {draws:>4} | {pct} | {pts:>5.1} |",
        name = e2_name,
        wins = s.e2_wins,
        losses = s.e1_wins,
        draws = s.draws,
        pct = format_pct(s.e2_wins * 2 + s.draws, total * 2),
        pts = e2_score,
        w = w,
    );
    println!(
        "  +-{0:-<w$}-+------+------+------+-------+-------+",
        "",
        w = w
    );
    println!();

    // White/Black breakdown
    let e1_as_white = s.e1_wins_as_white + s.e2_wins_as_white + s.draws_e1_white;
    let e1_as_black = s.e1_wins_as_black + s.e2_wins_as_black + s.draws_e1_black;

    println!("  White/Black Breakdown (from {}'s perspective):", e1_name);
    println!("  +-----------+------+------+------+-------+");
    println!(
        "  | Color     | {0:>4} | {1:>4} | {2:>4} | {3:>5} |",
        "W", "L", "D", "Score"
    );
    println!("  +-----------+------+------+------+-------+");
    if e1_as_white > 0 {
        println!(
            "  | As white  | {:>4} | {:>4} | {:>4} | {} |",
            s.e1_wins_as_white,
            s.e2_wins_as_white,
            s.draws_e1_white,
            format_pct(s.e1_wins_as_white * 2 + s.draws_e1_white, e1_as_white * 2),
        );
    }
    if e1_as_black > 0 {
        println!(
            "  | As black  | {:>4} | {:>4} | {:>4} | {} |",
            s.e1_wins_as_black,
            s.e2_wins_as_black,
            s.draws_e1_black,
            format_pct(s.e1_wins_as_black * 2 + s.draws_e1_black, e1_as_black * 2),
        );
    }
    println!("  +-----------+------+------+------+-------+");
    println!();

    // Termination stats
    println!("  Terminations:");
    let reasons: Vec<(&str, usize)> = vec![
        ("Checkmate", s.checkmates),
        ("Stalemate", s.stalemates),
        ("Repetition", s.repetitions),
        ("50-move rule", s.fifty_move),
        ("Insufficient material", s.insufficient_material),
        ("Flag fall", s.flag_falls),
        ("Move limit", s.move_limits),
        ("Illegal move", s.illegal_moves),
        ("Engine crash", s.crashes),
    ];
    for (name, count) in &reasons {
        if *count > 0 {
            println!(
                "    {:<21} {:>4}  ({})",
                name,
                count,
                format_pct(*count, total),
            );
        }
    }
    println!();

    // Game length stats
    println!("  Game Length:");
    println!(
        "    Average:  {:.0} plies ({:.0} moves)",
        avg_len,
        avg_len / 2.0
    );
    if s.shortest_game != usize::MAX {
        println!(
            "    Shortest: {} plies ({} moves)",
            s.shortest_game,
            s.shortest_game / 2
        );
    }
    println!(
        "    Longest:  {} plies ({} moves)",
        s.longest_game,
        s.longest_game / 2
    );
    println!();
    println!("=================================================================");
}

fn format_clock(clock: GameClock) -> String {
    format!("{}ms + {}ms/move", clock.base_ms, clock.increment_ms)
}

pub fn run_tournament(mut config: TournamentConfig) {
    if config.rounds % 2 == 1 {
        config.rounds += 1;
        println!(
            "Rounding games up to {} so every opening is played twice.",
            config.rounds
        );
    }

    if !std::path::Path::new(&config.engine1_path).exists() {
        eprintln!("Error: engine1 path '{}' not found", config.engine1_path);
        std::process::exit(1);
    }
    if !std::path::Path::new(&config.engine2_path).exists() {
        eprintln!("Error: engine2 path '{}' not found", config.engine2_path);
        std::process::exit(1);
    }

    let e1_name = config.e1_name.as_str();
    let e2_name = config.e2_name.as_str();

    println!("Tournament: {} vs {}", e1_name, e2_name);
    if config.e1_clock == config.e2_clock {
        println!("Time control: {}", format_clock(config.e1_clock));
    } else {
        println!(
            "Time control: {} {}, {} {}",
            e1_name,
            format_clock(config.e1_clock),
            e2_name,
            format_clock(config.e2_clock)
        );
    }
    println!(
        "Rounds: {}, Concurrency: {}",
        config.rounds, config.concurrency
    );
    if !config.openings.is_empty() {
        println!(
            "Openings: {} positions, each played as a color-swapped pair",
            config.openings.len()
        );
    } else if config.start.fen() != START_FEN {
        println!("Start FEN: {}", config.start.fen());
    }
    if let Some(sprt) = config.sprt {
        println!(
            "SPRT: elo0={} elo1={} alpha={} beta={} bounds=[{:.2}, {:.2}]",
            sprt.elo0,
            sprt.elo1,
            sprt.alpha,
            sprt.beta,
            sprt.lower_bound(),
            sprt.upper_bound()
        );
    }
    println!("---");

    let start_time = Instant::now();
    let next_game = Arc::new(AtomicUsize::new(0));
    let score = Arc::new(Mutex::new(Score::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let config = Arc::new(config);
    let mut handles = Vec::new();

    for _ in 0..config.concurrency {
        let next_game = Arc::clone(&next_game);
        let score = Arc::clone(&score);
        let stop = Arc::clone(&stop);
        let config = Arc::clone(&config);

        let handle = thread::spawn(move || {
            let mut e1 = match spawn_and_init_pinned(&config.engine1_path, &config.e1_options, None)
            {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("Failed to start engine1: {}", e);
                    return;
                }
            };
            let mut e2 = match spawn_and_init_pinned(&config.engine2_path, &config.e2_options, None)
            {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("Failed to start engine2: {}", e);
                    return;
                }
            };

            let e1_name = config.e1_name.as_str();
            let e2_name = config.e2_name.as_str();

            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let game_idx = next_game.fetch_add(1, Ordering::Relaxed);
                if game_idx >= config.rounds {
                    break;
                }

                let e1_is_white = game_idx.is_multiple_of(2);
                let (white_clock, black_clock) = if e1_is_white {
                    (config.e1_clock, config.e2_clock)
                } else {
                    (config.e2_clock, config.e1_clock)
                };
                let game_config = GameConfig {
                    white_clock,
                    black_clock,
                    max_moves: 300,
                    start: config.start_for_game(game_idx).clone(),
                };

                let (white, black) = if e1_is_white {
                    (&mut e1, &mut e2)
                } else {
                    (&mut e2, &mut e1)
                };

                let (white_name, black_name) = if e1_is_white {
                    (e1_name, e2_name)
                } else {
                    (e2_name, e1_name)
                };

                let outcome = play_game(white, black, &game_config);

                let (e1_win, e2_win) = match outcome.result {
                    GameResult::WhiteWin => (e1_is_white, !e1_is_white),
                    GameResult::BlackWin => (!e1_is_white, e1_is_white),
                    GameResult::Draw => (false, false),
                };

                let result_str = match outcome.result {
                    GameResult::WhiteWin => "1-0",
                    GameResult::BlackWin => "0-1",
                    GameResult::Draw => "1/2-1/2",
                };

                let mut s = score.lock().unwrap();

                // Basic score
                if e1_win {
                    s.e1_wins += 1;
                } else if e2_win {
                    s.e2_wins += 1;
                } else {
                    s.draws += 1;
                }
                s.completed += 1;

                // White/Black breakdown
                if e1_is_white {
                    if e1_win {
                        s.e1_wins_as_white += 1;
                    } else if e2_win {
                        s.e2_wins_as_white += 1;
                    } else {
                        s.draws_e1_white += 1;
                    }
                } else {
                    if e1_win {
                        s.e1_wins_as_black += 1;
                    } else if e2_win {
                        s.e2_wins_as_black += 1;
                    } else {
                        s.draws_e1_black += 1;
                    }
                }

                // Termination reason
                match outcome.reason {
                    GameEndReason::Checkmate => s.checkmates += 1,
                    GameEndReason::Stalemate => s.stalemates += 1,
                    GameEndReason::Repetition => s.repetitions += 1,
                    GameEndReason::FiftyMoveRule => s.fifty_move += 1,
                    GameEndReason::InsufficientMaterial => s.insufficient_material += 1,
                    GameEndReason::FlagFall => s.flag_falls += 1,
                    GameEndReason::MoveLimitExceeded => s.move_limits += 1,
                    GameEndReason::IllegalMove => s.illegal_moves += 1,
                    GameEndReason::EngineCrash => s.crashes += 1,
                }

                // Game length
                s.total_plies += outcome.move_count;
                if outcome.move_count < s.shortest_game {
                    s.shortest_game = outcome.move_count;
                }
                if outcome.move_count > s.longest_game {
                    s.longest_game = outcome.move_count;
                }

                let e1_points = if e1_win {
                    1.0
                } else if e2_win {
                    0.0
                } else {
                    0.5
                };
                let pair_completed = s.record_pair_game(game_idx, e1_points);

                let e1_score = s.e1_wins as f64 + s.draws as f64 * 0.5;
                let e2_score = s.e2_wins as f64 + s.draws as f64 * 0.5;
                let elo = format_elo(s.e1_wins, s.e2_wins, s.draws);

                println!(
                    "Game {:>3}/{}: {} vs {} -> {} ({}, {} plies)  [{} {:.1} - {:.1} {}] [{}]",
                    s.completed,
                    config.rounds,
                    white_name,
                    black_name,
                    result_str,
                    outcome.reason,
                    outcome.move_count,
                    e1_name,
                    e1_score,
                    e2_score,
                    e2_name,
                    elo,
                );

                if pair_completed
                    && let Some(sprt) = config.sprt
                    && s.sprt_decision.is_none()
                {
                    let llr = sprt.llr(&s.penta);
                    println!(
                        "  SPRT: LLR {:.2} [{:.2}, {:.2}] after {} pairs",
                        llr,
                        sprt.lower_bound(),
                        sprt.upper_bound(),
                        s.penta.pairs()
                    );
                    let decision = sprt.decide(llr);
                    if decision != SprtDecision::Continue {
                        s.sprt_decision = Some(decision);
                        stop.store(true, Ordering::Relaxed);
                    }
                }

                // If an engine crashed, try to respawn it
                if outcome.reason == GameEndReason::EngineCrash {
                    if let Ok(new_e1) =
                        spawn_and_init_pinned(&config.engine1_path, &config.e1_options, None)
                    {
                        e1 = new_e1;
                    }
                    if let Ok(new_e2) =
                        spawn_and_init_pinned(&config.engine2_path, &config.e2_options, None)
                    {
                        e2 = new_e2;
                    }
                }
            }

            e1.quit();
            e2.quit();
        });

        handles.push(handle);
    }

    for h in handles {
        let _ = h.join();
    }

    let elapsed = start_time.elapsed().as_secs_f64();

    let mut s = score.lock().unwrap();
    if config.sprt.is_some() && s.sprt_decision.is_none() {
        s.sprt_decision = Some(SprtDecision::Continue);
    }
    let e1_name = config.e1_name.as_str();
    let e2_name = config.e2_name.as_str();

    print_summary(&s, e1_name, e2_name, elapsed);
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPENING_A: &str = "rnbqkbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNBQKBNR w KQkq - 0 2";
    const OPENING_B: &str = "rnbqkbnr/pppppppp/8/8/3P4/8/PPP1PPPP/RNBQKBNR b KQkq - 0 1";

    fn position(fen: &str) -> StartPosition {
        StartPosition::parse(fen).unwrap()
    }

    fn config_with(openings: &[&str], start: &str) -> TournamentConfig {
        let clock = GameClock {
            base_ms: 1000,
            increment_ms: 10,
        };
        TournamentConfig {
            engine1_path: String::new(),
            engine2_path: String::new(),
            e1_options: Vec::new(),
            e2_options: Vec::new(),
            e1_name: "one".to_string(),
            e2_name: "two".to_string(),
            e1_clock: clock,
            e2_clock: clock,
            rounds: 1,
            concurrency: 1,
            start: position(start),
            openings: openings.iter().map(|o| position(o)).collect(),
            sprt: None,
        }
    }

    #[test]
    fn elo_is_undefined_without_games_or_with_a_clean_sweep() {
        assert_eq!(elo_diff(0, 0, 0), None);
        assert_eq!(elo_diff(5, 0, 0), None);
        assert_eq!(elo_diff(0, 5, 0), None);
    }

    #[test]
    fn elo_follows_the_logistic_curve() {
        let even = elo_diff(3, 3, 4).unwrap();
        let three_quarters = elo_diff(3, 1, 0).unwrap();

        assert!(even.abs() < 1e-9, "{even}");
        assert!(
            (three_quarters - 400.0 * 3f64.log10()).abs() < 1e-9,
            "{three_quarters}"
        );
    }

    #[test]
    fn the_error_bar_needs_two_games_and_a_mixed_score() {
        assert_eq!(elo_error_95(1, 0, 0), None);
        assert_eq!(elo_error_95(0, 0, 0), None);
        assert_eq!(elo_error_95(4, 0, 0), None);
        assert!(elo_error_95(10, 10, 0).is_some_and(|e| e.is_finite() && e > 0.0));
    }

    #[test]
    fn a_clean_sweep_formats_as_infinite_elo() {
        let shown = [
            format_elo(4, 0, 0),
            format_elo(0, 4, 0),
            format_elo(0, 0, 0),
            format_elo(3, 1, 0),
        ];

        assert_eq!(shown, ["+inf", "-inf", "0", "+191"]);
    }

    #[test]
    fn the_two_games_of_a_pair_count_once_whichever_finishes_first() {
        let mut score = Score::new();

        let first_halves = [
            score.record_pair_game(3, 1.0),
            score.record_pair_game(0, 0.5),
        ];
        let second_halves = [
            score.record_pair_game(2, 1.0),
            score.record_pair_game(1, 0.0),
        ];

        assert_eq!(first_halves, [false, false]);
        assert_eq!(second_halves, [true, true]);
        assert_eq!(score.penta.counts(), [0, 1, 0, 0, 1]);
    }

    #[test]
    fn game_pairs_cycle_through_the_openings() {
        let config = config_with(&[OPENING_A, OPENING_B], START_FEN);

        let fens: Vec<_> = (0..6)
            .map(|game| config.start_for_game(game).fen())
            .collect();

        assert_eq!(
            fens,
            [
                OPENING_A, OPENING_A, OPENING_B, OPENING_B, OPENING_A, OPENING_A
            ]
        );
    }

    #[test]
    fn without_openings_every_game_starts_from_the_start_fen() {
        let config = config_with(&[], OPENING_B);

        assert_eq!(config.start_for_game(5).fen(), OPENING_B);
    }
}
