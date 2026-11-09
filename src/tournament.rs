use crate::game_runner::{
    GameClock, GameConfig, GameEndReason, GameOutcome, GameResult, StartPosition, play_game,
};
use crate::sprt::{PairScore, Pentanomial, SprtConfig, SprtDecision};
use crate::types::START_FEN;
use crate::uci_client::UciEngine;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
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

/// The moves after which a game is called a draw.
const MAX_GAME_MOVES: usize = 300;

/// Engine 1 plays white in the first game of each pair and black in the second.
const fn e1_plays_white(game_idx: usize) -> bool {
    game_idx.is_multiple_of(2)
}

impl TournamentConfig {
    fn game_config(&self, game_idx: usize) -> GameConfig {
        let (white_clock, black_clock) = if e1_plays_white(game_idx) {
            (self.e1_clock, self.e2_clock)
        } else {
            (self.e2_clock, self.e1_clock)
        };
        GameConfig {
            white_clock,
            black_clock,
            max_moves: MAX_GAME_MOVES,
            start: self.start_for_game(game_idx).clone(),
        }
    }

    fn start_for_game(&self, game_idx: usize) -> &StartPosition {
        if self.openings.is_empty() {
            return &self.start;
        }
        let pair_idx = game_idx / 2;
        &self.openings[pair_idx % self.openings.len()]
    }
}

/// How a game went for one engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngineResult {
    Win,
    Loss,
    Draw,
}

impl EngineResult {
    /// The result for the engine that played white when `is_white`, else black.
    const fn of(result: GameResult, is_white: bool) -> Self {
        match (result, is_white) {
            (GameResult::Draw, _) => Self::Draw,
            (GameResult::WhiteWin, true) | (GameResult::BlackWin, false) => Self::Win,
            (GameResult::WhiteWin, false) | (GameResult::BlackWin, true) => Self::Loss,
        }
    }

    const fn points(self) -> f64 {
        match self {
            Self::Win => 1.0,
            Self::Draw => 0.5,
            Self::Loss => 0.0,
        }
    }
}

/// Wins, losses and draws from one engine's side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Wdl {
    wins: usize,
    losses: usize,
    draws: usize,
}

impl Wdl {
    fn add(&mut self, result: EngineResult) {
        match result {
            EngineResult::Win => self.wins += 1,
            EngineResult::Loss => self.losses += 1,
            EngineResult::Draw => self.draws += 1,
        }
    }

    const fn games(self) -> usize {
        self.wins + self.losses + self.draws
    }

    fn points(self) -> f64 {
        self.wins as f64 + self.draws as f64 * 0.5
    }

    /// The same games from the opponent's side.
    const fn opponent(self) -> Self {
        Self {
            wins: self.losses,
            losses: self.wins,
            draws: self.draws,
        }
    }
}

/// The standings so far, from engine 1's side.
#[derive(Default)]
struct Score {
    e1: Wdl,
    e1_as_white: Wdl,
    e1_as_black: Wdl,
    terminations: HashMap<GameEndReason, usize>,
    total_plies: usize,
    shortest_game: Option<usize>,
    longest_game: usize,
    /// Game pairs: the same opening with colours swapped.
    penta: Pentanomial,
    unpaired_e1_points: HashMap<usize, f64>,
    sprt_decision: Option<SprtDecision>,
}

impl Score {
    /// Counts game `game_idx`, played by engine 1 as white when `e1_is_white`;
    /// returns true when it completes a pair.
    fn record(&mut self, outcome: &GameOutcome, e1_is_white: bool, game_idx: usize) -> bool {
        let e1_result = EngineResult::of(outcome.result, e1_is_white);
        self.e1.add(e1_result);
        if e1_is_white {
            self.e1_as_white.add(e1_result);
        } else {
            self.e1_as_black.add(e1_result);
        }
        *self.terminations.entry(outcome.reason).or_default() += 1;
        self.total_plies += outcome.move_count;
        self.shortest_game = Some(self.shortest_game.map_or(outcome.move_count, |shortest| {
            shortest.min(outcome.move_count)
        }));
        self.longest_game = self.longest_game.max(outcome.move_count);
        self.record_pair_game(game_idx, e1_result.points())
    }

    fn completed(&self) -> usize {
        self.e1.games()
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
        // `{:+.0}` would show an estimate that rounds to zero as "+0" or "-0".
        Some(e) if e.round() == 0.0 => "0".to_string(),
        Some(e) => format!("{:+.0}", e),
        None if wins > losses => "+inf".to_string(),
        None if losses > wins => "-inf".to_string(),
        _ => "0".to_string(),
    }
}

/// `n` as a percentage of `total`, six characters wide.
fn format_pct(n: usize, total: usize) -> String {
    if total == 0 {
        format!("{:>6}", "-")
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
    println!();
    println!("{SUMMARY_RULE}");
    println!("  TOURNAMENT RESULTS");
    println!("{SUMMARY_RULE}");
    println!();
    print_score_lines(s, elapsed);
    print_head_to_head(s, e1_name, e2_name);
    print_colour_breakdown(s, e1_name);
    print_terminations(s);
    print_game_lengths(s);
    println!("{SUMMARY_RULE}");
}

const SUMMARY_RULE: &str = "=================================================================";

/// The score with its Elo estimate, then the game, pair, SPRT and speed counts.
fn print_score_lines(s: &Score, elapsed: f64) {
    let total = s.completed();
    let elo = format_elo(s.e1.wins, s.e1.losses, s.e1.draws);
    let err = match s.penta.elo() {
        Some((_, pair_error)) => Some(pair_error),
        None => elo_error_95(s.e1.wins, s.e1.losses, s.e1.draws),
    };
    let draw_rate = if total > 0 {
        s.e1.draws as f64 / total as f64 * 100.0
    } else {
        0.0
    };
    let elo_str = match err {
        Some(e) => format!("{} +/- {:.0} Elo", elo, e),
        None => format!("{} Elo", elo),
    };
    println!(
        "  Score: {:.1} - {:.1}  [{elo_str}]",
        s.e1.points(),
        s.e1.opponent().points()
    );
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
}

fn print_head_to_head(s: &Score, e1_name: &str, e2_name: &str) {
    let w = e1_name.len().max(e2_name.len()).max(6);
    let border = format!("  +-{0:-<w$}-+------+------+------+--------+-------+", "");
    println!("{border}");
    println!(
        "  | {0:<w$} | {1:>4} | {2:>4} | {3:>4} | {4:>6} | {5:>5} |",
        "Engine", "W", "L", "D", "Score", "Pts",
    );
    println!("{border}");
    print_head_to_head_row(e1_name, s.e1, w);
    print_head_to_head_row(e2_name, s.e1.opponent(), w);
    println!("{border}");
    println!();
}

fn print_head_to_head_row(name: &str, wdl: Wdl, w: usize) {
    println!(
        "  | {name:<w$} | {wins:>4} | {losses:>4} | {draws:>4} | {pct} | {pts:>5.1} |",
        wins = wdl.wins,
        losses = wdl.losses,
        draws = wdl.draws,
        pct = format_pct(wdl.wins * 2 + wdl.draws, wdl.games() * 2),
        pts = wdl.points(),
    );
}

/// Engine 1's results as white and as black.
fn print_colour_breakdown(s: &Score, e1_name: &str) {
    let border = "  +-----------+------+------+------+--------+";
    println!("  White/Black Breakdown (from {}'s perspective):", e1_name);
    println!("{border}");
    println!(
        "  | Color     | {0:>4} | {1:>4} | {2:>4} | {3:>6} |",
        "W", "L", "D", "Score"
    );
    println!("{border}");
    print_colour_row("As white", s.e1_as_white);
    print_colour_row("As black", s.e1_as_black);
    println!("{border}");
    println!();
}

fn print_colour_row(label: &str, wdl: Wdl) {
    if wdl.games() == 0 {
        return;
    }
    println!(
        "  | {label:<9} | {:>4} | {:>4} | {:>4} | {} |",
        wdl.wins,
        wdl.losses,
        wdl.draws,
        format_pct(wdl.wins * 2 + wdl.draws, wdl.games() * 2),
    );
}

/// How many games ended each way, in a fixed order.
fn print_terminations(s: &Score) {
    const ENDINGS: [(&str, GameEndReason); 9] = [
        ("Checkmate", GameEndReason::Checkmate),
        ("Stalemate", GameEndReason::Stalemate),
        ("Repetition", GameEndReason::Repetition),
        ("50-move rule", GameEndReason::FiftyMoveRule),
        ("Insufficient material", GameEndReason::InsufficientMaterial),
        ("Flag fall", GameEndReason::FlagFall),
        ("Move limit", GameEndReason::MoveLimitExceeded),
        ("Illegal move", GameEndReason::IllegalMove),
        ("Engine crash", GameEndReason::EngineCrash),
    ];
    println!("  Terminations:");
    for (name, reason) in ENDINGS {
        let count = s.terminations.get(&reason).copied().unwrap_or(0);
        if count > 0 {
            println!(
                "    {:<21} {:>4}  ({})",
                name,
                count,
                format_pct(count, s.completed()),
            );
        }
    }
    println!();
}

fn print_game_lengths(s: &Score) {
    let total = s.completed();
    let avg_len = if total > 0 {
        s.total_plies as f64 / total as f64
    } else {
        0.0
    };
    println!("  Game Length:");
    println!(
        "    Average:  {:.0} plies ({:.0} moves)",
        avg_len,
        avg_len / 2.0
    );
    if let Some(shortest) = s.shortest_game {
        println!("    Shortest: {} plies ({} moves)", shortest, shortest / 2);
    }
    println!(
        "    Longest:  {} plies ({} moves)",
        s.longest_game,
        s.longest_game / 2
    );
    println!();
}

fn format_clock(clock: GameClock) -> String {
    format!("{}ms + {}ms/move", clock.base_ms, clock.increment_ms)
}

/// Plays the tournament and prints its results, or says why it cannot start.
pub fn run_tournament(mut config: TournamentConfig) -> Result<(), String> {
    round_up_to_pairs(&mut config);
    check_engine_paths(&config)?;
    print_header(&config);

    let start_time = Instant::now();
    let tournament = Tournament::new(config);
    thread::scope(|scope| {
        for _ in 0..tournament.config.concurrency {
            scope.spawn(|| run_worker(&tournament));
        }
    });
    let elapsed = start_time.elapsed().as_secs_f64();

    let Tournament { config, score, .. } = tournament;
    let mut score = score.into_inner().unwrap_or_else(PoisonError::into_inner);
    if config.sprt.is_some() && score.sprt_decision.is_none() {
        score.sprt_decision = Some(SprtDecision::Continue);
    }
    print_summary(&score, &config.e1_name, &config.e2_name, elapsed);
    Ok(())
}

/// Makes the game count even, so every opening is played with both colours.
fn round_up_to_pairs(config: &mut TournamentConfig) {
    if config.rounds % 2 == 1 {
        config.rounds += 1;
        println!(
            "Rounding games up to {} so every opening is played twice.",
            config.rounds
        );
    }
}

fn check_engine_paths(config: &TournamentConfig) -> Result<(), String> {
    for (engine, path) in [
        ("engine1", &config.engine1_path),
        ("engine2", &config.engine2_path),
    ] {
        if !std::path::Path::new(path).exists() {
            return Err(format!("{engine} path '{path}' not found"));
        }
    }
    Ok(())
}

fn print_header(config: &TournamentConfig) {
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
}

/// What a tournament's workers share: the configuration, the next game to
/// hand out, the standings, and whether SPRT has ended the tournament.
struct Tournament {
    config: TournamentConfig,
    next_game: AtomicUsize,
    score: Mutex<Score>,
    stop: AtomicBool,
}

impl Tournament {
    fn new(config: TournamentConfig) -> Self {
        Self {
            config,
            next_game: AtomicUsize::new(0),
            score: Mutex::new(Score::default()),
            stop: AtomicBool::new(false),
        }
    }

    /// The next game to play, or `None` once all are handed out or SPRT has
    /// decided.
    fn claim_game(&self) -> Option<usize> {
        if self.stop.load(Ordering::Relaxed) {
            return None;
        }
        let game_idx = self.next_game.fetch_add(1, Ordering::Relaxed);
        (game_idx < self.config.rounds).then_some(game_idx)
    }

    /// Adds a finished game to the standings and prints it, then runs SPRT
    /// if the game completes a pair.
    fn record(&self, game_idx: usize, outcome: &GameOutcome) {
        let e1_is_white = e1_plays_white(game_idx);
        let mut score = self.score.lock().unwrap_or_else(PoisonError::into_inner);
        let pair_completed = score.record(outcome, e1_is_white, game_idx);
        print_game_line(&self.config, &score, e1_is_white, outcome);
        if pair_completed {
            self.check_sprt(&mut score);
        }
    }

    /// Prints the log-likelihood ratio and stops the tournament once SPRT
    /// accepts a hypothesis.
    fn check_sprt(&self, score: &mut Score) {
        let Some(sprt) = self.config.sprt else {
            return;
        };
        if score.sprt_decision.is_some() {
            return;
        }
        let llr = sprt.llr(&score.penta);
        println!(
            "  SPRT: LLR {:.2} [{:.2}, {:.2}] after {} pairs",
            llr,
            sprt.lower_bound(),
            sprt.upper_bound(),
            score.penta.pairs()
        );
        let decision = sprt.decide(llr);
        if decision != SprtDecision::Continue {
            score.sprt_decision = Some(decision);
            self.stop.store(true, Ordering::Relaxed);
        }
    }
}

fn print_game_line(
    config: &TournamentConfig,
    score: &Score,
    e1_is_white: bool,
    outcome: &GameOutcome,
) {
    let (e1_name, e2_name) = (config.e1_name.as_str(), config.e2_name.as_str());
    let (white_name, black_name) = if e1_is_white {
        (e1_name, e2_name)
    } else {
        (e2_name, e1_name)
    };
    let result_str = match outcome.result {
        GameResult::WhiteWin => "1-0",
        GameResult::BlackWin => "0-1",
        GameResult::Draw => "1/2-1/2",
    };
    println!(
        "Game {:>3}/{}: {} vs {} -> {} ({}, {} plies)  [{} {:.1} - {:.1} {}] [{}]",
        score.completed(),
        config.rounds,
        white_name,
        black_name,
        result_str,
        outcome.reason,
        outcome.move_count,
        e1_name,
        score.e1.points(),
        score.e1.opponent().points(),
        e2_name,
        format_elo(score.e1.wins, score.e1.losses, score.e1.draws),
    );
}

/// Plays games with one pair of engines until the tournament runs out of
/// them. After a crash both engines restart; if that fails the worker stops,
/// since every game on a dead engine would only be another crash.
fn run_worker(tournament: &Tournament) {
    let config = &tournament.config;
    let (mut e1, mut e2) = match start_engines(config) {
        Ok(engines) => engines,
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    };

    while let Some(game_idx) = tournament.claim_game() {
        let (white, black) = if e1_plays_white(game_idx) {
            (&mut e1, &mut e2)
        } else {
            (&mut e2, &mut e1)
        };
        let outcome = play_game(white, black, &config.game_config(game_idx));
        tournament.record(game_idx, &outcome);

        if outcome.reason == GameEndReason::EngineCrash {
            (e1, e2) = match start_engines(config) {
                Ok(engines) => engines,
                Err(e) => {
                    eprintln!("{e}; this worker plays no more games");
                    return;
                }
            };
        }
    }

    e1.quit();
    e2.quit();
}

fn start_engines(config: &TournamentConfig) -> Result<(UciEngine, UciEngine), String> {
    let e1 = spawn_and_init_pinned(&config.engine1_path, &config.e1_options, None)
        .map_err(|e| format!("Failed to start engine1: {e}"))?;
    let e2 = spawn_and_init_pinned(&config.engine2_path, &config.e2_options, None)
        .map_err(|e| format!("Failed to start engine2: {e}"))?;
    Ok((e1, e2))
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
    fn an_even_or_nearly_even_score_formats_as_zero_elo() {
        let shown = [format_elo(2, 2, 0), format_elo(1000, 1001, 0)];

        assert_eq!(shown, ["0", "0"]);
    }

    #[test]
    fn percentages_fill_the_six_character_score_column() {
        let shown = [format_pct(1, 3), format_pct(3, 3), format_pct(0, 0)];

        assert_eq!(shown, [" 33.3%", "100.0%", "     -"]);
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

    fn outcome(result: GameResult, reason: GameEndReason, move_count: usize) -> GameOutcome {
        GameOutcome {
            result,
            reason,
            move_count,
        }
    }

    #[test]
    fn a_game_result_reads_from_each_engines_side() {
        let results = [
            EngineResult::of(GameResult::WhiteWin, true),
            EngineResult::of(GameResult::WhiteWin, false),
            EngineResult::of(GameResult::BlackWin, true),
            EngineResult::of(GameResult::BlackWin, false),
            EngineResult::of(GameResult::Draw, true),
            EngineResult::of(GameResult::Draw, false),
        ];

        use EngineResult::{Draw, Loss, Win};
        assert_eq!(results, [Win, Loss, Loss, Win, Draw, Draw]);
    }

    #[test]
    fn recorded_games_count_by_colour_ending_and_length() {
        let mut score = Score::default();

        let first = score.record(
            &outcome(GameResult::WhiteWin, GameEndReason::Checkmate, 40),
            true,
            0,
        );
        let second = score.record(
            &outcome(GameResult::WhiteWin, GameEndReason::FlagFall, 10),
            false,
            1,
        );

        let wdl = |wins, losses, draws| Wdl {
            wins,
            losses,
            draws,
        };
        assert_eq!((first, second), (false, true));
        assert_eq!(score.e1, wdl(1, 1, 0));
        assert_eq!(
            (score.e1_as_white, score.e1_as_black),
            (wdl(1, 0, 0), wdl(0, 1, 0))
        );
        assert_eq!(score.terminations[&GameEndReason::Checkmate], 1);
        assert_eq!(score.terminations[&GameEndReason::FlagFall], 1);
        assert_eq!(
            (score.shortest_game, score.longest_game, score.total_plies),
            (Some(10), 40, 50)
        );
        assert_eq!(score.penta.counts(), [0, 0, 1, 0, 0]);
    }

    #[test]
    fn a_tournament_without_games_has_no_shortest_game() {
        assert_eq!(Score::default().shortest_game, None);
    }

    #[test]
    fn the_two_games_of_a_pair_count_once_whichever_finishes_first() {
        let mut score = Score::default();

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

    /// Answers the UCI handshake and dies on its first `go`; every later
    /// start fails at once.
    const CRASHES_THEN_WONT_START: &str = r#"#!/bin/sh
marker="$0.started"
[ -e "$marker" ] && exit 1
: > "$marker"
while read -r line; do
    case "$line" in
        uci) echo uciok ;;
        isready) echo readyok ;;
        go*) exit 0 ;;
        quit) exit 0 ;;
    esac
done
"#;

    /// Answers the UCI handshake and plays 1. e4 whenever asked to move.
    const PLAYS_E4: &str = r#"#!/bin/sh
while read -r line; do
    case "$line" in
        uci) echo uciok ;;
        isready) echo readyok ;;
        go*) echo "bestmove e2e4" ;;
        quit) exit 0 ;;
    esac
done
"#;

    #[cfg(unix)]
    fn write_engine(dir: &std::path::Path, name: &str, script: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn a_worker_stops_when_a_crashed_engine_cannot_be_restarted() {
        let dir = std::env::temp_dir().join(format!("chess-respawn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut config = config_with(&[], START_FEN);
        config.engine1_path = write_engine(&dir, "crashing", CRASHES_THEN_WONT_START);
        config.engine2_path = write_engine(&dir, "e4", PLAYS_E4);
        config.rounds = 4;
        let tournament = Tournament::new(config);

        run_worker(&tournament);

        let score = tournament.score.into_inner().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(score.completed(), 1);
        assert_eq!(score.terminations[&GameEndReason::EngineCrash], 1);
    }
}
