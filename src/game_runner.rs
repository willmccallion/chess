use crate::board::Board;
use crate::types::{Color, MoveList, START_FEN};
use crate::uci_client::UciEngine;
use crate::uci_io::parse_uci_move;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameResult {
    WhiteWin,
    BlackWin,
    Draw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameEndReason {
    Checkmate,
    Stalemate,
    Repetition,
    FiftyMoveRule,
    InsufficientMaterial,
    MoveLimitExceeded,
    IllegalMove,
    EngineCrash,
    FlagFall,
}

/// How a game went for one engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EngineResult {
    Win,
    Loss,
    Draw,
}

impl EngineResult {
    /// The result for the engine that played white when `is_white`, else black.
    pub(crate) const fn of(result: GameResult, is_white: bool) -> Self {
        match (result, is_white) {
            (GameResult::Draw, _) => Self::Draw,
            (GameResult::WhiteWin, true) | (GameResult::BlackWin, false) => Self::Win,
            (GameResult::WhiteWin, false) | (GameResult::BlackWin, true) => Self::Loss,
        }
    }

    pub(crate) const fn points(self) -> f64 {
        match self {
            Self::Win => 1.0,
            Self::Draw => 0.5,
            Self::Loss => 0.0,
        }
    }

    /// +1 for a win, 0 for a draw and -1 for a loss.
    pub(crate) const fn signed(self) -> i32 {
        match self {
            Self::Win => 1,
            Self::Draw => 0,
            Self::Loss => -1,
        }
    }
}

/// Wins, losses and draws from one engine's side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Wdl {
    pub(crate) wins: usize,
    pub(crate) losses: usize,
    pub(crate) draws: usize,
}

impl Wdl {
    pub(crate) fn add(&mut self, result: EngineResult) {
        match result {
            EngineResult::Win => self.wins += 1,
            EngineResult::Loss => self.losses += 1,
            EngineResult::Draw => self.draws += 1,
        }
    }

    pub(crate) const fn games(self) -> usize {
        self.wins + self.losses + self.draws
    }

    pub(crate) fn points(self) -> f64 {
        self.wins as f64 + self.draws as f64 * 0.5
    }

    /// The same games from the opponent's side.
    pub(crate) const fn opponent(self) -> Self {
        Self {
            wins: self.losses,
            losses: self.wins,
            draws: self.draws,
        }
    }
}

pub struct GameOutcome {
    pub result: GameResult,
    pub reason: GameEndReason,
    pub move_count: usize,
}

/// One side's clock: starting time and the increment added after each move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameClock {
    pub base_ms: u64,
    pub increment_ms: u64,
}

/// A position a game can start from: a FEN that reads as a legal board.
#[derive(Clone)]
pub struct StartPosition {
    fen: String,
    board: Board,
}

impl StartPosition {
    /// The position `fen` describes, or why it describes none.
    pub fn parse(fen: &str) -> Result<Self, String> {
        let board =
            Board::from_fen(fen).map_err(|e| format!("invalid start position '{fen}': {e}"))?;
        Ok(Self {
            fen: fen.to_string(),
            board,
        })
    }

    /// The position as a FEN.
    pub fn fen(&self) -> &str {
        &self.fen
    }
}

pub struct GameConfig {
    pub white_clock: GameClock,
    pub black_clock: GameClock,
    pub max_moves: usize,
    pub start: StartPosition,
}

impl std::fmt::Display for GameEndReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Checkmate => write!(f, "checkmate"),
            Self::Stalemate => write!(f, "stalemate"),
            Self::Repetition => write!(f, "repetition"),
            Self::FiftyMoveRule => write!(f, "50-move rule"),
            Self::InsufficientMaterial => write!(f, "insufficient material"),
            Self::MoveLimitExceeded => write!(f, "move limit"),
            Self::IllegalMove => write!(f, "illegal move"),
            Self::EngineCrash => write!(f, "engine crash"),
            Self::FlagFall => write!(f, "flag fall"),
        }
    }
}

fn report_rejected_move(engine: &str, bestmove: &str, board: &Board) {
    eprintln!(
        "{engine} played illegal move '{bestmove}' in {}",
        board.to_fen()
    );
}

/// The result if the game is over in `board` by the rules: mate, stalemate, threefold
/// repetition, the 50-move rule or insufficient mating material.
pub fn position_outcome(board: &Board) -> Option<(GameResult, GameEndReason)> {
    let mut legal = MoveList::new();
    board.generate_legal_moves(&mut legal);
    if legal.is_empty() {
        return Some(if board.in_check() {
            (win_for(board.turn.other()), GameEndReason::Checkmate)
        } else {
            (GameResult::Draw, GameEndReason::Stalemate)
        });
    }
    let draw_reason = if board.is_draw_by_repetition() {
        GameEndReason::Repetition
    } else if board.halfmove_clock >= 100 {
        GameEndReason::FiftyMoveRule
    } else if board.has_insufficient_material() {
        GameEndReason::InsufficientMaterial
    } else {
        return None;
    };
    Some((GameResult::Draw, draw_reason))
}

/// The result when `winner` wins.
pub const fn win_for(winner: Color) -> GameResult {
    match winner {
        Color::White => GameResult::WhiteWin,
        Color::Black => GameResult::BlackWin,
    }
}

pub fn play_game(white: &mut UciEngine, black: &mut UciEngine, config: &GameConfig) -> GameOutcome {
    start_new_game(white, "white");
    start_new_game(black, "black");
    let mut game = Game::new(config);
    loop {
        if let Some((result, reason)) = game.result_by_rules(config.max_moves) {
            return game.outcome(result, reason);
        }
        let engine = match game.board.turn {
            Color::White => &mut *white,
            Color::Black => &mut *black,
        };
        if let Err(reason) = game.play_turn(engine) {
            return game.outcome(win_for(game.board.turn.other()), reason);
        }
    }
}

fn start_new_game(engine: &mut UciEngine, side: &str) {
    if let Err(e) = engine.new_game() {
        eprintln!("Warning: {side} engine new_game failed: {e}");
    }
}

/// Both sides' time left and the increment each gains after a move.
struct Clocks {
    white_ms: i64,
    black_ms: i64,
    white_increment_ms: u64,
    black_increment_ms: u64,
}

impl Clocks {
    fn new(config: &GameConfig) -> Self {
        Self {
            white_ms: config.white_clock.base_ms as i64,
            black_ms: config.black_clock.base_ms as i64,
            white_increment_ms: config.white_clock.increment_ms,
            black_increment_ms: config.black_clock.increment_ms,
        }
    }

    /// Charges `side` for a move that took `elapsed_ms`, then adds its
    /// increment, unless its flag fell first.
    fn charge(&mut self, side: Color, elapsed_ms: i64) -> Result<(), GameEndReason> {
        let (left_ms, increment_ms) = match side {
            Color::White => (&mut self.white_ms, self.white_increment_ms),
            Color::Black => (&mut self.black_ms, self.black_increment_ms),
        };
        *left_ms -= elapsed_ms;
        if *left_ms <= 0 {
            return Err(GameEndReason::FlagFall);
        }
        *left_ms += increment_ms as i64;
        Ok(())
    }
}

/// A game in progress: the position, the moves that reached it and the clocks.
struct Game<'a> {
    /// The start FEN to send engines, or `None` for the standard start.
    start_fen: Option<&'a str>,
    board: Board,
    moves: Vec<String>,
    clocks: Clocks,
}

impl<'a> Game<'a> {
    fn new(config: &'a GameConfig) -> Self {
        Self {
            start_fen: (config.start.fen() != START_FEN).then(|| config.start.fen()),
            board: config.start.board.clone(),
            moves: Vec::new(),
            clocks: Clocks::new(config),
        }
    }

    /// The result if the rules or the move limit have ended the game.
    fn result_by_rules(&self, max_moves: usize) -> Option<(GameResult, GameEndReason)> {
        if self.moves.len() / 2 >= max_moves {
            return Some((GameResult::Draw, GameEndReason::MoveLimitExceeded));
        }
        position_outcome(&self.board)
    }

    fn outcome(&self, result: GameResult, reason: GameEndReason) -> GameOutcome {
        GameOutcome {
            result,
            reason,
            move_count: self.moves.len(),
        }
    }

    /// Asks `engine` for the side to move's move and plays it, or says why
    /// that side loses instead.
    fn play_turn(&mut self, engine: &mut UciEngine) -> Result<(), GameEndReason> {
        engine
            .set_position(self.start_fen, &self.moves)
            .map_err(|_| GameEndReason::EngineCrash)?;
        let start = Instant::now();
        let c = &self.clocks;
        let bestmove = engine
            .go(
                c.white_ms,
                c.black_ms,
                c.white_increment_ms,
                c.black_increment_ms,
            )
            .map_err(|_| GameEndReason::EngineCrash)?;
        self.clocks
            .charge(self.board.turn, start.elapsed().as_millis() as i64)?;
        let Some(m) = parse_uci_move(&self.board, &bestmove) else {
            report_rejected_move(&engine.name, &bestmove, &self.board);
            return Err(GameEndReason::IllegalMove);
        };
        self.board.make_move(m);
        self.moves.push(bestmove);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome_of(fen: &str) -> Option<(GameResult, GameEndReason)> {
        position_outcome(&Board::from_fen(fen).unwrap())
    }

    #[test]
    fn a_mated_side_loses() {
        let fools_mate = "rnb1kbnr/pppp1ppp/8/4p3/6Pq/5P2/PPPPP2P/RNBQKBNR w KQkq - 1 3";

        assert_eq!(
            outcome_of(fools_mate),
            Some((GameResult::BlackWin, GameEndReason::Checkmate))
        );
    }

    #[test]
    fn a_side_with_no_move_and_no_check_is_stalemated() {
        assert_eq!(
            outcome_of("7k/5Q2/6K1/8/8/8/8/8 b - - 0 1"),
            Some((GameResult::Draw, GameEndReason::Stalemate))
        );
    }

    #[test]
    fn the_third_occurrence_of_a_position_is_a_draw_and_the_second_is_not() {
        let mut board = Board::from_fen(START_FEN).unwrap();
        let mut outcomes = Vec::new();
        for text in [
            "g1f3", "g8f6", "f3g1", "f6g8", "g1f3", "g8f6", "f3g1", "f6g8",
        ] {
            let m = parse_uci_move(&board, text).unwrap();
            board.make_move(m);
            outcomes.push(position_outcome(&board));
        }

        assert_eq!(outcomes[3], None, "the start position's second occurrence");
        assert_eq!(
            outcomes[7],
            Some((GameResult::Draw, GameEndReason::Repetition))
        );
    }

    #[test]
    fn a_hundred_plies_without_capture_or_pawn_move_is_a_draw() {
        assert_eq!(
            outcome_of("4k3/8/8/8/8/8/R7/4K3 w - - 100 80"),
            Some((GameResult::Draw, GameEndReason::FiftyMoveRule))
        );
    }

    #[test]
    fn a_lone_knight_cannot_mate() {
        assert_eq!(
            outcome_of("4k3/8/8/8/8/8/8/4K1N1 w - - 0 1"),
            Some((GameResult::Draw, GameEndReason::InsufficientMaterial))
        );
    }

    #[test]
    fn a_position_with_moves_and_material_goes_on() {
        assert_eq!(outcome_of(START_FEN), None);
        assert_eq!(outcome_of("4k3/8/8/8/8/8/R7/4K3 w - - 99 80"), None);
    }
}
