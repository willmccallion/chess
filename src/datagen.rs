//! Self-play games for training a network. Each kept position is written as
//! `<FEN> | <score> | <result>`, with the search score in centipawns and the game result
//! (1.0, 0.5 or 0.0) both from White's side: the text format bullet converts to its data.

use crate::board::Board;
use crate::game_runner::{GameResult, position_outcome};
use crate::search::{
    NodeLimit, Score, SearchControl, SearchLimits, SearchThreads, search_reporting,
};
use crate::search_params::SearchParams;
use crate::tt::SharedTransTable;
use crate::types::{Color, Move, MoveList, START_FEN};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::fs::OpenOptions;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const MAX_DEPTH: usize = 64;
/// Openings the search already scores beyond this are too lopsided to learn from.
const MAX_OPENING_SCORE_CP: i32 = 500;
/// Positions scored beyond this are not kept; mate scores never are.
const MAX_KEPT_SCORE_CP: i32 = 3000;
const MAX_GAME_PLIES: usize = 400;
const WIN_SCORE_CP: i32 = 2000;
const WIN_PLIES: usize = 4;
const DRAW_SCORE_CP: i32 = 10;
const DRAW_PLIES: usize = 8;
const DRAW_MIN_PLY: usize = 60;
const PROGRESS_INTERVAL: Duration = Duration::from_secs(10);

pub struct DatagenConfig {
    /// Created by the run; an existing file is never overwritten.
    pub output: PathBuf,
    /// Stop once this many positions are written.
    pub positions: u64,
    pub threads: usize,
    pub nodes: NodeLimit,
    /// Uniformly random moves played from the start position before the engine takes over;
    /// one more in half the games, so neither side always makes the last random move.
    pub random_plies: usize,
    pub seed: u64,
    pub hash_mb: usize,
}

/// One finished game's kept positions and result.
struct GameRecord {
    positions: Vec<(String, i32)>,
    result: GameResult,
}

pub struct DatagenSummary {
    pub games: u64,
    pub positions: u64,
    pub elapsed: Duration,
}

/// Plays games on `config.threads` threads until enough positions are written.
pub fn run(config: &DatagenConfig) -> io::Result<DatagenSummary> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&config.output)?;
    let mut out = BufWriter::new(file);
    let stop = AtomicBool::new(false);
    let (sender, receiver) = mpsc::sync_channel::<GameRecord>(256);
    let start = Instant::now();
    let mut tally = Tally::default();

    thread::scope(|scope| -> io::Result<()> {
        for worker in 0..config.threads.max(1) {
            let sender = sender.clone();
            let stop = &stop;
            let seed = config.seed ^ (worker as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            thread::Builder::new()
                .name(format!("datagen-{worker}"))
                .spawn_scoped(scope, move || play_games(config, seed, stop, &sender))?;
        }
        drop(sender);

        let mut last_report = Instant::now();
        for game in &receiver {
            write_game(&mut out, &game)?;
            tally.add(&game);
            if last_report.elapsed() >= PROGRESS_INTERVAL {
                eprintln!("{}", tally.progress(start.elapsed()));
                last_report = Instant::now();
            }
            if tally.positions >= config.positions {
                stop.store(true, Ordering::Relaxed);
            }
        }
        Ok(())
    })?;

    out.flush()?;
    eprintln!("{}", tally.progress(start.elapsed()));
    Ok(DatagenSummary {
        games: tally.games,
        positions: tally.positions,
        elapsed: start.elapsed(),
    })
}

fn play_games(
    config: &DatagenConfig,
    seed: u64,
    stop: &AtomicBool,
    sender: &mpsc::SyncSender<GameRecord>,
) {
    let mut rng = StdRng::seed_from_u64(seed);
    let tt = SharedTransTable::new(config.hash_mb);
    let mut threads = SearchThreads::new(1);
    while !stop.load(Ordering::Relaxed) {
        tt.clear();
        threads.clear();
        let Some(game) = play_game(&mut rng, &tt, &mut threads, config) else {
            continue;
        };
        if sender.send(game).is_err() {
            return;
        }
    }
}

fn result_text(result: GameResult) -> &'static str {
    match result {
        GameResult::WhiteWin => "1.0",
        GameResult::Draw => "0.5",
        GameResult::BlackWin => "0.0",
    }
}

fn write_game(out: &mut impl Write, game: &GameRecord) -> io::Result<()> {
    for (fen, score) in &game.positions {
        writeln!(out, "{fen} | {score} | {}", result_text(game.result))?;
    }
    Ok(())
}

/// The engine's verdict on a position, from White's side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Centipawns(i32),
    /// White mates (true) or is mated (false).
    Mate(bool),
}

impl Verdict {
    fn from_search(score: Score, turn: Color) -> Self {
        let sign = if turn == Color::White { 1 } else { -1 };
        match score {
            Score::Centipawns(cp) => Self::Centipawns(sign * cp),
            Score::MateIn(moves) => Self::Mate((moves > 0) == (turn == Color::White)),
        }
    }
}

/// Ends games whose outcome is no longer in doubt.
#[derive(Default)]
struct Adjudicator {
    winning_streak: usize,
    winning_side: Option<bool>,
    quiet_streak: usize,
}

impl Adjudicator {
    /// Records the verdict at `ply`; returns a result once the game can be called.
    fn observe(&mut self, verdict: Verdict, ply: usize) -> Option<GameResult> {
        let leader = match verdict {
            Verdict::Mate(white_wins) => Some(white_wins),
            Verdict::Centipawns(cp) if cp.abs() >= WIN_SCORE_CP => Some(cp > 0),
            Verdict::Centipawns(_) => None,
        };
        if leader.is_some() && leader == self.winning_side {
            self.winning_streak += 1;
        } else {
            self.winning_streak = usize::from(leader.is_some());
        }
        self.winning_side = leader;
        if self.winning_streak >= WIN_PLIES {
            return Some(if leader == Some(true) {
                GameResult::WhiteWin
            } else {
                GameResult::BlackWin
            });
        }

        let quiet = matches!(verdict, Verdict::Centipawns(cp) if cp.abs() <= DRAW_SCORE_CP);
        self.quiet_streak = if quiet && ply >= DRAW_MIN_PLY {
            self.quiet_streak + 1
        } else {
            0
        };
        (self.quiet_streak >= DRAW_PLIES).then_some(GameResult::Draw)
    }
}

fn random_opening(rng: &mut StdRng, plies: usize) -> Option<Board> {
    let mut board = Board::from_fen(START_FEN).ok()?;
    for _ in 0..plies {
        let mut moves = MoveList::new();
        board.generate_legal_moves(&mut moves);
        if moves.is_empty() {
            return None;
        }
        board.make_move(moves.get(rng.gen_range(0..moves.len())));
    }
    position_outcome(&board).is_none().then_some(board)
}

fn in_check(board: &Board) -> bool {
    let king = board.king_square(board.turn);
    king < 64 && board.is_square_attacked(king as i32, board.turn.other())
}

/// Positions whose best move changes material teach the network little about quiet play.
fn is_quiet(m: Move) -> bool {
    !m.capture && m.promotion.is_none()
}

fn think(
    board: &Board,
    tt: &SharedTransTable,
    threads: &mut SearchThreads,
    nodes: NodeLimit,
) -> Option<(Move, Score)> {
    let limits = SearchLimits {
        max_depth: MAX_DEPTH,
        params: SearchParams::default(),
        wait_for_stop: false,
        nodes: Some(nodes),
    };
    let control = SearchControl::new(None);
    let result = search_reporting(board, tt, limits, threads, &control, &|_| {});
    Some((result.best_move?, result.score?))
}

fn play_game(
    rng: &mut StdRng,
    tt: &SharedTransTable,
    threads: &mut SearchThreads,
    config: &DatagenConfig,
) -> Option<GameRecord> {
    let plies = config.random_plies + rng.gen_range(0..=1);
    let mut board = random_opening(rng, plies)?;
    let (_, opening_score) = think(&board, tt, threads, config.nodes)?;
    if !matches!(opening_score, Score::Centipawns(cp) if cp.abs() <= MAX_OPENING_SCORE_CP) {
        return None;
    }

    let mut positions = Vec::new();
    let mut adjudicator = Adjudicator::default();
    for ply in 0..MAX_GAME_PLIES {
        if let Some((result, _)) = position_outcome(&board) {
            return Some(GameRecord { positions, result });
        }
        let (best, score) = think(&board, tt, threads, config.nodes)?;
        let verdict = Verdict::from_search(score, board.turn);
        if let Verdict::Centipawns(cp) = verdict
            && cp.abs() <= MAX_KEPT_SCORE_CP
            && is_quiet(best)
            && !in_check(&board)
        {
            positions.push((board.to_fen(), cp));
        }
        if let Some(result) = adjudicator.observe(verdict, ply) {
            return Some(GameRecord { positions, result });
        }
        board.make_move(best);
    }
    Some(GameRecord {
        positions,
        result: GameResult::Draw,
    })
}

#[derive(Default)]
struct Tally {
    games: u64,
    positions: u64,
    white_wins: u64,
    draws: u64,
    black_wins: u64,
}

impl Tally {
    fn add(&mut self, game: &GameRecord) {
        self.games += 1;
        self.positions += game.positions.len() as u64;
        match game.result {
            GameResult::WhiteWin => self.white_wins += 1,
            GameResult::Draw => self.draws += 1,
            GameResult::BlackWin => self.black_wins += 1,
        }
    }

    fn progress(&self, elapsed: Duration) -> String {
        let seconds = elapsed.as_secs_f64().max(1e-9);
        let share = |n: u64| 100.0 * n as f64 / self.games.max(1) as f64;
        format!(
            "{} games, {} positions ({:.0}/s), white {:.1}% draw {:.1}% black {:.1}%",
            self.games,
            self.positions,
            self.positions as f64 / seconds,
            share(self.white_wins),
            share(self.draws),
            share(self.black_wins),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lasting_big_advantage_wins_and_a_brief_one_does_not() {
        let mut lasting = Adjudicator::default();
        let mut brief = Adjudicator::default();
        let big = Verdict::Centipawns(-2500);

        let lasting_results: Vec<_> = (0..WIN_PLIES)
            .map(|ply| lasting.observe(big, ply))
            .collect();
        brief.observe(big, 0);
        brief.observe(big, 1);
        let after_recovery = brief.observe(Verdict::Centipawns(50), 2);

        assert_eq!(lasting_results.last(), Some(&Some(GameResult::BlackWin)));
        assert!(lasting_results[..WIN_PLIES - 1].iter().all(Option::is_none));
        assert_eq!(after_recovery, None);
    }

    #[test]
    fn level_endgames_are_drawn_only_after_the_opening_phase() {
        let mut early = Adjudicator::default();
        let mut late = Adjudicator::default();
        let level = Verdict::Centipawns(3);

        let early_result = (0..DRAW_PLIES)
            .map(|ply| early.observe(level, ply))
            .last()
            .flatten();
        let late_result = (0..DRAW_PLIES)
            .map(|ply| late.observe(level, DRAW_MIN_PLY + ply))
            .last()
            .flatten();

        assert_eq!(early_result, None);
        assert_eq!(late_result, Some(GameResult::Draw));
    }

    #[test]
    fn mate_scores_are_turned_into_whites_point_of_view() {
        assert_eq!(
            Verdict::from_search(Score::MateIn(2), Color::Black),
            Verdict::Mate(false)
        );
        assert_eq!(
            Verdict::from_search(Score::MateIn(-2), Color::Black),
            Verdict::Mate(true)
        );
        assert_eq!(
            Verdict::from_search(Score::Centipawns(40), Color::Black),
            Verdict::Centipawns(-40)
        );
    }

    #[test]
    fn generated_games_write_parseable_lines_for_bullet() {
        crate::eval::EvalOptions::built_in().load().unwrap();
        let config = DatagenConfig {
            output: PathBuf::new(),
            positions: 0,
            threads: 1,
            nodes: NodeLimit {
                soft: 300,
                hard: 3000,
            },
            random_plies: 8,
            seed: 1,
            hash_mb: 1,
        };
        let tt = SharedTransTable::new(config.hash_mb);
        let mut threads = SearchThreads::new(1);
        let mut rng = StdRng::seed_from_u64(config.seed);
        let game = (0..20)
            .find_map(|_| play_game(&mut rng, &tt, &mut threads, &config))
            .unwrap();

        let mut text = Vec::new();
        write_game(&mut text, &game).unwrap();
        let text = String::from_utf8(text).unwrap();

        assert!(!game.positions.is_empty());
        for line in text.lines() {
            let fields: Vec<&str> = line.split(" | ").collect();
            assert_eq!(fields.len(), 3, "{line}");
            Board::from_fen(fields[0]).unwrap();
            let score: i16 = fields[1].parse().unwrap();
            assert!(i32::from(score).abs() <= MAX_KEPT_SCORE_CP);
            assert!(["1.0", "0.5", "0.0"].contains(&fields[2]));
        }
    }
}
