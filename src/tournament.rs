use crate::elo::{elo_error_95, elo_of};
use crate::game_runner::{
    EngineResult, GameClock, GameConfig, GameEndReason, GameOutcome, GameResult, StartPosition,
    Wdl, play_game,
};
use crate::sprt::{PairScore, Pentanomial, SprtConfig, SprtDecision};
use crate::types::{Color, START_FEN};
use crate::uci_client::{EngineError, UciEngine, engine_display_name};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::Instant;

/// One side of a tournament: the engine binary, the UCI options it is given,
/// the name it is shown under and its clock.
pub struct EngineSpec {
    pub path: PathBuf,
    pub options: Vec<(String, String)>,
    pub name: String,
    pub clock: GameClock,
}

impl EngineSpec {
    fn start(&self) -> Result<UciEngine, EngineError> {
        start_match_engine(&self.path, &self.options, None)
    }
}

pub struct TournamentConfig {
    pub engine1: EngineSpec,
    pub engine2: EngineSpec,
    pub rounds: usize,
    pub concurrency: usize,
    /// Where every game starts when there are no openings.
    pub start: StartPosition,
    /// Start positions; game pair `p` plays `openings[p % len]` with colors swapped.
    pub openings: Vec<StartPosition>,
    pub sprt: Option<SprtConfig>,
}

/// The name a tournament shows for an engine: `name` if given, else the binary's file name.
pub fn engine_name(name: Option<String>, path: &Path) -> String {
    name.unwrap_or_else(|| engine_display_name(path))
}

/// The moves after which a game is called a draw.
const MAX_GAME_MOVES: usize = 300;

/// Engine 1 plays white in the first game of each pair and black in the second.
const fn e1_color(game_idx: usize) -> Color {
    if game_idx.is_multiple_of(2) {
        Color::White
    } else {
        Color::Black
    }
}

impl TournamentConfig {
    fn game_config(&self, game_idx: usize) -> GameConfig {
        let (white_clock, black_clock) = match e1_color(game_idx) {
            Color::White => (self.engine1.clock, self.engine2.clock),
            Color::Black => (self.engine2.clock, self.engine1.clock),
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

/// The standings so far, from engine 1's side.
#[derive(Default)]
struct Standings {
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

impl Standings {
    /// Counts game `game_idx`, played by engine 1 as `e1_color`; returns true
    /// when it completes a pair.
    fn record(&mut self, outcome: &GameOutcome, e1_color: Color, game_idx: usize) -> bool {
        let e1_result = EngineResult::of(outcome.result, e1_color);
        self.e1.add(e1_result);
        match e1_color {
            Color::White => self.e1_as_white.add(e1_result),
            Color::Black => self.e1_as_black.add(e1_result),
        }
        *self.terminations.entry(outcome.reason).or_default() += 1;
        self.total_plies += outcome.move_count;
        self.shortest_game = Some(self.shortest_game.map_or(outcome.move_count, |shortest| {
            shortest.min(outcome.move_count)
        }));
        self.longest_game = self.longest_game.max(outcome.move_count);
        self.record_pair_game(game_idx, e1_result.points())
    }

    const fn completed(&self) -> usize {
        self.e1.games()
    }

    /// Records engine 1's points for a game; returns true when this completes a pair.
    fn record_pair_game(&mut self, game_idx: usize, e1_points: f64) -> bool {
        let pair_idx = game_idx / 2;
        if let Some(partner_points) = self.unpaired_e1_points.remove(&pair_idx) {
            self.penta
                .add(PairScore::from_game_points(partner_points, e1_points));
            true
        } else {
            self.unpaired_e1_points.insert(pair_idx, e1_points);
            false
        }
    }
}

pub fn parse_option(s: &str) -> Result<(String, String), String> {
    let (name, value) = s
        .split_once('=')
        .ok_or_else(|| format!("Invalid option '{s}', expected Name=Value"))?;
    Ok((name.trim().to_string(), value.trim().to_string()))
}

fn format_elo(wdl: Wdl) -> String {
    match elo_of(wdl) {
        // `{:+.0}` would show an estimate that rounds to zero as "+0" or "-0".
        Some(e) if e.round() == 0.0 => "0".to_string(),
        Some(e) => format!("{e:+.0}"),
        None if wdl.wins > wdl.losses => "+inf".to_string(),
        None if wdl.losses > wdl.wins => "-inf".to_string(),
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
/// Starts an engine to play match games: on one search thread unless
/// `options` set Threads.
pub(crate) fn start_match_engine(
    path: &Path,
    options: &[(String, String)],
    cpus: Option<&str>,
) -> Result<UciEngine, EngineError> {
    let sets_threads = options
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("Threads"));
    let one_thread = (!sets_threads).then(|| ("Threads".to_string(), "1".to_string()));
    let options: Vec<_> = one_thread
        .into_iter()
        .chain(options.iter().cloned())
        .collect();
    UciEngine::start(path, &options, cpus)
}

fn print_summary(standings: &Standings, e1_name: &str, e2_name: &str, elapsed: f64) {
    println!();
    println!("{SUMMARY_RULE}");
    println!("  TOURNAMENT RESULTS");
    println!("{SUMMARY_RULE}");
    println!();
    print_score_lines(standings, elapsed);
    print_head_to_head(standings, e1_name, e2_name);
    print_colour_breakdown(standings, e1_name);
    print_terminations(standings);
    print_game_lengths(standings);
    println!("{SUMMARY_RULE}");
}

const SUMMARY_RULE: &str = "=================================================================";

/// The score with its Elo estimate, then the game, pair, SPRT and speed counts.
fn print_score_lines(standings: &Standings, elapsed: f64) {
    let total = standings.completed();
    let elo = format_elo(standings.e1);
    let err = match standings.penta.elo() {
        Some((_, pair_error)) => Some(pair_error),
        None => elo_error_95(standings.e1),
    };
    let draw_rate = if total > 0 {
        standings.e1.draws as f64 / total as f64 * 100.0
    } else {
        0.0
    };
    let elo_str = match err {
        Some(e) => format!("{elo} +/- {e:.0} Elo"),
        None => format!("{elo} Elo"),
    };
    println!(
        "  Score: {:.1} - {:.1}  [{elo_str}]",
        standings.e1.points(),
        standings.e1.opponent().points()
    );
    println!("  Games: {total}    Draw rate: {draw_rate:.1}%");
    if standings.penta.pairs() > 0 {
        let [ll, ld, even, wd, ww] = standings.penta.counts();
        println!(
            "  Pairs: {}    Pentanomial [LL LD DD/WL WD WW]: [{} {} {} {} {}]",
            standings.penta.pairs(),
            ll,
            ld,
            even,
            wd,
            ww
        );
    }
    if let Some(decision) = standings.sprt_decision {
        let verdict = match decision {
            SprtDecision::AcceptH1 => "H1 accepted (engine1 is stronger)",
            SprtDecision::AcceptH0 => "H0 accepted (no gain shown)",
            SprtDecision::Continue => "inconclusive (game limit reached)",
        };
        println!("  SPRT:  {verdict}");
    }
    if elapsed > 0.0 {
        let gps = total as f64 / elapsed;
        println!("  Time:  {elapsed:.1}s ({gps:.1} games/sec)");
    }
    println!();
}

fn print_head_to_head(standings: &Standings, e1_name: &str, e2_name: &str) {
    let w = e1_name.len().max(e2_name.len()).max(6);
    let border = format!("  +-{0:-<w$}-+------+------+------+--------+-------+", "");
    println!("{border}");
    println!(
        "  | {0:<w$} | {1:>4} | {2:>4} | {3:>4} | {4:>6} | {5:>5} |",
        "Engine", "W", "L", "D", "Score", "Pts",
    );
    println!("{border}");
    print_head_to_head_row(e1_name, standings.e1, w);
    print_head_to_head_row(e2_name, standings.e1.opponent(), w);
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
fn print_colour_breakdown(standings: &Standings, e1_name: &str) {
    let border = "  +-----------+------+------+------+--------+";
    println!("  White/Black Breakdown (from {e1_name}'s perspective):");
    println!("{border}");
    println!(
        "  | Color     | {0:>4} | {1:>4} | {2:>4} | {3:>6} |",
        "W", "L", "D", "Score"
    );
    println!("{border}");
    print_colour_row("As white", standings.e1_as_white);
    print_colour_row("As black", standings.e1_as_black);
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
fn print_terminations(standings: &Standings) {
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
        let count = standings.terminations.get(&reason).copied().unwrap_or(0);
        if count > 0 {
            println!(
                "    {:<21} {:>4}  ({})",
                name,
                count,
                format_pct(count, standings.completed()),
            );
        }
    }
    println!();
}

fn print_game_lengths(standings: &Standings) {
    let total = standings.completed();
    let avg_len = if total > 0 {
        standings.total_plies as f64 / total as f64
    } else {
        0.0
    };
    println!("  Game Length:");
    println!(
        "    Average:  {:.0} plies ({:.0} moves)",
        avg_len,
        avg_len / 2.0
    );
    if let Some(shortest) = standings.shortest_game {
        println!("    Shortest: {} plies ({} moves)", shortest, shortest / 2);
    }
    println!(
        "    Longest:  {} plies ({} moves)",
        standings.longest_game,
        standings.longest_game / 2
    );
    println!();
}

fn format_clock(clock: GameClock) -> String {
    format!("{}ms + {}ms/move", clock.base, clock.increment)
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

    let Tournament {
        config, standings, ..
    } = tournament;
    let mut standings = standings
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    if config.sprt.is_some() && standings.sprt_decision.is_none() {
        standings.sprt_decision = Some(SprtDecision::Continue);
    }
    print_summary(
        &standings,
        &config.engine1.name,
        &config.engine2.name,
        elapsed,
    );
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
        ("engine1", &config.engine1.path),
        ("engine2", &config.engine2.path),
    ] {
        if !path.exists() {
            return Err(format!("{engine} path '{}' not found", path.display()));
        }
    }
    Ok(())
}

fn print_header(config: &TournamentConfig) {
    let (e1, e2) = (&config.engine1, &config.engine2);
    println!("Tournament: {} vs {}", e1.name, e2.name);
    if e1.clock == e2.clock {
        println!("Time control: {}", format_clock(e1.clock));
    } else {
        println!(
            "Time control: {} {}, {} {}",
            e1.name,
            format_clock(e1.clock),
            e2.name,
            format_clock(e2.clock)
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
    standings: Mutex<Standings>,
    stop: AtomicBool,
}

impl Tournament {
    fn new(config: TournamentConfig) -> Self {
        Self {
            config,
            next_game: AtomicUsize::new(0),
            standings: Mutex::new(Standings::default()),
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
    #[allow(
        clippy::significant_drop_tightening,
        reason = "holding the lock keeps each printed line in step with its standings"
    )]
    fn record(&self, game_idx: usize, outcome: &GameOutcome) {
        let e1_color = e1_color(game_idx);
        let mut standings = self
            .standings
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let pair_completed = standings.record(outcome, e1_color, game_idx);
        print_game_line(&self.config, &standings, e1_color, outcome);
        if pair_completed {
            self.check_sprt(&mut standings);
        }
    }

    /// Prints the log-likelihood ratio and stops the tournament once SPRT
    /// accepts a hypothesis.
    fn check_sprt(&self, standings: &mut Standings) {
        let Some(sprt) = self.config.sprt else {
            return;
        };
        if standings.sprt_decision.is_some() {
            return;
        }
        let llr = sprt.llr(&standings.penta);
        println!(
            "  SPRT: LLR {:.2} [{:.2}, {:.2}] after {} pairs",
            llr,
            sprt.lower_bound(),
            sprt.upper_bound(),
            standings.penta.pairs()
        );
        let decision = sprt.decide(llr);
        if decision != SprtDecision::Continue {
            standings.sprt_decision = Some(decision);
            self.stop.store(true, Ordering::Relaxed);
        }
    }
}

fn print_game_line(
    config: &TournamentConfig,
    standings: &Standings,
    e1_color: Color,
    outcome: &GameOutcome,
) {
    let (e1_name, e2_name) = (config.engine1.name.as_str(), config.engine2.name.as_str());
    let (white_name, black_name) = match e1_color {
        Color::White => (e1_name, e2_name),
        Color::Black => (e2_name, e1_name),
    };
    let result_str = match outcome.result {
        GameResult::WhiteWin => "1-0",
        GameResult::BlackWin => "0-1",
        GameResult::Draw => "1/2-1/2",
    };
    println!(
        "Game {:>3}/{}: {} vs {} -> {} ({}, {} plies)  [{} {:.1} - {:.1} {}] [{}]",
        standings.completed(),
        config.rounds,
        white_name,
        black_name,
        result_str,
        outcome.reason,
        outcome.move_count,
        e1_name,
        standings.e1.points(),
        standings.e1.opponent().points(),
        e2_name,
        format_elo(standings.e1),
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
        let (white, black) = match e1_color(game_idx) {
            Color::White => (&mut e1, &mut e2),
            Color::Black => (&mut e2, &mut e1),
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
    let e1 = config
        .engine1
        .start()
        .map_err(|e| format!("Failed to start engine1: {e}"))?;
    let e2 = config
        .engine2
        .start()
        .map_err(|e| format!("Failed to start engine2: {e}"))?;
    Ok((e1, e2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Millis;

    const OPENING_A: &str = "rnbqkbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNBQKBNR w KQkq - 0 2";
    const OPENING_B: &str = "rnbqkbnr/pppppppp/8/8/3P4/8/PPP1PPPP/RNBQKBNR b KQkq - 0 1";

    fn position(fen: &str) -> StartPosition {
        StartPosition::parse(fen).unwrap()
    }

    fn engine(name: &str, clock: GameClock) -> EngineSpec {
        EngineSpec {
            path: PathBuf::new(),
            options: Vec::new(),
            name: name.to_string(),
            clock,
        }
    }

    fn config_with(openings: &[&str], start: &str) -> TournamentConfig {
        let clock = GameClock {
            base: Millis::new(1000),
            increment: Millis::new(10),
        };
        TournamentConfig {
            engine1: engine("one", clock),
            engine2: engine("two", clock),
            rounds: 1,
            concurrency: 1,
            start: position(start),
            openings: openings.iter().map(|o| position(o)).collect(),
            sprt: None,
        }
    }

    #[test]
    fn an_even_or_nearly_even_score_formats_as_zero_elo() {
        let shown = [format_elo(wdl(2, 2, 0)), format_elo(wdl(1000, 1001, 0))];

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
            format_elo(wdl(4, 0, 0)),
            format_elo(wdl(0, 4, 0)),
            format_elo(wdl(0, 0, 0)),
            format_elo(wdl(3, 1, 0)),
        ];

        assert_eq!(shown, ["+inf", "-inf", "0", "+191"]);
    }

    const fn wdl(wins: usize, losses: usize, draws: usize) -> Wdl {
        Wdl {
            wins,
            losses,
            draws,
        }
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
        use EngineResult::{Draw, Loss, Win};
        let results = [
            EngineResult::of(GameResult::WhiteWin, Color::White),
            EngineResult::of(GameResult::WhiteWin, Color::Black),
            EngineResult::of(GameResult::BlackWin, Color::White),
            EngineResult::of(GameResult::BlackWin, Color::Black),
            EngineResult::of(GameResult::Draw, Color::White),
            EngineResult::of(GameResult::Draw, Color::Black),
        ];

        assert_eq!(results, [Win, Loss, Loss, Win, Draw, Draw]);
    }

    #[test]
    fn recorded_games_count_by_colour_ending_and_length() {
        let mut standings = Standings::default();

        let first = standings.record(
            &outcome(GameResult::WhiteWin, GameEndReason::Checkmate, 40),
            Color::White,
            0,
        );
        let second = standings.record(
            &outcome(GameResult::WhiteWin, GameEndReason::FlagFall, 10),
            Color::Black,
            1,
        );

        assert_eq!((first, second), (false, true));
        assert_eq!(standings.e1, wdl(1, 1, 0));
        assert_eq!(
            (standings.e1_as_white, standings.e1_as_black),
            (wdl(1, 0, 0), wdl(0, 1, 0))
        );
        assert_eq!(standings.terminations[&GameEndReason::Checkmate], 1);
        assert_eq!(standings.terminations[&GameEndReason::FlagFall], 1);
        assert_eq!(
            (
                standings.shortest_game,
                standings.longest_game,
                standings.total_plies
            ),
            (Some(10), 40, 50)
        );
        assert_eq!(standings.penta.counts(), [0, 0, 1, 0, 0]);
    }

    #[test]
    fn a_tournament_without_games_has_no_shortest_game() {
        assert_eq!(Standings::default().shortest_game, None);
    }

    #[test]
    fn the_two_games_of_a_pair_count_once_whichever_finishes_first() {
        let mut standings = Standings::default();

        let first_halves = [
            standings.record_pair_game(3, 1.0),
            standings.record_pair_game(0, 0.5),
        ];
        let second_halves = [
            standings.record_pair_game(2, 1.0),
            standings.record_pair_game(1, 0.0),
        ];

        assert_eq!(first_halves, [false, false]);
        assert_eq!(second_halves, [true, true]);
        assert_eq!(standings.penta.counts(), [0, 1, 0, 0, 1]);
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
    fn write_engine(dir: &Path, name: &str, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn a_worker_stops_when_a_crashed_engine_cannot_be_restarted() {
        let dir = std::env::temp_dir().join(format!("chess-respawn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut config = config_with(&[], START_FEN);
        config.engine1.path = write_engine(&dir, "crashing", CRASHES_THEN_WONT_START);
        config.engine2.path = write_engine(&dir, "e4", PLAYS_E4);
        config.rounds = 4;
        let tournament = Tournament::new(config);

        run_worker(&tournament);

        let standings = tournament.standings.into_inner().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(standings.completed(), 1);
        assert_eq!(standings.terminations[&GameEndReason::EngineCrash], 1);
    }
}
