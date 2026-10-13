use crate::board::Board;
use crate::types::{Color, MoveList, Piece, PieceKind, START_FEN};
use crate::uci_client::UciEngine;
use crate::uci_io::parse_uci_move;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameResult {
    WhiteWin,
    BlackWin,
    Draw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

pub struct GameConfig {
    pub white_clock: GameClock,
    pub black_clock: GameClock,
    pub max_moves: usize,
    pub start_fen: Option<String>,
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
    eprintln!("{engine} played illegal move '{bestmove}' in {}", board.to_fen());
}

/// The result if the game is over in `board` by the rules: mate, stalemate, threefold
/// repetition, the 50-move rule or insufficient mating material.
pub fn position_outcome(board: &Board) -> Option<(GameResult, GameEndReason)> {
    let mut legal = MoveList::new();
    board.generate_legal_moves(&mut legal);
    if legal.is_empty() {
        let king = Piece::from_kind(PieceKind::King, board.turn);
        let king_sq = board.piece_bb[king.index()].trailing_zeros();
        let in_check =
            king_sq < 64 && board.is_square_attacked(king_sq as i32, board.turn.other());
        return Some(if in_check {
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

pub fn play_game(
    white: &mut UciEngine,
    black: &mut UciEngine,
    config: &GameConfig,
) -> GameOutcome {
    if let Err(e) = white.new_game() {
        eprintln!("Warning: white engine new_game failed: {}", e);
    }
    if let Err(e) = black.new_game() {
        eprintln!("Warning: black engine new_game failed: {}", e);
    }

    let fen_str = config.start_fen.as_deref().unwrap_or(START_FEN);
    let mut board = match Board::from_fen(fen_str) {
        Ok(b) => b,
        Err(_) => {
            return GameOutcome {
                result: GameResult::Draw,
                reason: GameEndReason::EngineCrash,
                move_count: 0,
            };
        }
    };

    let is_startpos = config.start_fen.is_none();
    let mut wtime = config.white_clock.base_ms as i64;
    let mut btime = config.black_clock.base_ms as i64;
    let (winc, binc) = (config.white_clock.increment_ms, config.black_clock.increment_ms);
    let mut moves: Vec<String> = Vec::new();
    let mut ply = 0usize;

    loop {
        // Check move limit
        if ply / 2 >= config.max_moves {
            return GameOutcome {
                result: GameResult::Draw,
                reason: GameEndReason::MoveLimitExceeded,
                move_count: ply,
            };
        }

        if let Some((result, reason)) = position_outcome(&board) {
            return GameOutcome {
                result,
                reason,
                move_count: ply,
            };
        }

        let engine = if board.turn == Color::White {
            &mut *white
        } else {
            &mut *black
        };

        // Send position
        let start_fen_arg = if is_startpos { None } else { Some(fen_str) };
        if let Err(_) = engine.set_position(start_fen_arg, &moves) {
            let result = if board.turn == Color::White {
                GameResult::BlackWin
            } else {
                GameResult::WhiteWin
            };
            return GameOutcome {
                result,
                reason: GameEndReason::EngineCrash,
                move_count: ply,
            };
        }

        // Send go and measure time
        let start = Instant::now();
        let bestmove = match engine.go(wtime, btime, winc, binc) {
            Ok(m) => m,
            Err(_) => {
                let result = if board.turn == Color::White {
                    GameResult::BlackWin
                } else {
                    GameResult::WhiteWin
                };
                return GameOutcome {
                    result,
                    reason: GameEndReason::EngineCrash,
                    move_count: ply,
                };
            }
        };
        let elapsed_ms = start.elapsed().as_millis() as i64;

        // Deduct time
        let (clock, increment_ms) = if board.turn == Color::White {
            (&mut wtime, winc)
        } else {
            (&mut btime, binc)
        };
        *clock -= elapsed_ms;
        if *clock <= 0 {
            let result = if board.turn == Color::White {
                GameResult::BlackWin
            } else {
                GameResult::WhiteWin
            };
            return GameOutcome {
                result,
                reason: GameEndReason::FlagFall,
                move_count: ply,
            };
        }
        *clock += increment_ms as i64;

        // Handle "(none)" or "0000"
        if bestmove == "(none)" || bestmove == "0000" {
            report_rejected_move(&engine.name, &bestmove, &board);
            let result = if board.turn == Color::White {
                GameResult::BlackWin
            } else {
                GameResult::WhiteWin
            };
            return GameOutcome {
                result,
                reason: GameEndReason::IllegalMove,
                move_count: ply,
            };
        }

        // Validate and apply move
        let mv = parse_uci_move(&mut board, &bestmove);
        match mv {
            Some(m) => {
                let _ = board.make_move(m);
                moves.push(bestmove);
                ply += 1;
            }
            None => {
                report_rejected_move(&engine.name, &bestmove, &board);
                let result = if board.turn == Color::White {
                    GameResult::BlackWin
                } else {
                    GameResult::WhiteWin
                };
                return GameOutcome {
                    result,
                    reason: GameEndReason::IllegalMove,
                    move_count: ply,
                };
            }
        }
    }
}
