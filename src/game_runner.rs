use crate::board::Board;
use crate::types::{Color, MoveList, Piece, PieceKind, START_FEN};
use crate::uci_client::UciEngine;
use crate::uci_io::parse_uci_move;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GameResult {
    WhiteWin,
    BlackWin,
    Draw,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GameEndReason {
    Checkmate,
    Stalemate,
    Repetition,
    FiftyMoveRule,
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

pub struct GameConfig {
    pub base_time_ms: u64,
    pub increment_ms: u64,
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
            Self::MoveLimitExceeded => write!(f, "move limit"),
            Self::IllegalMove => write!(f, "illegal move"),
            Self::EngineCrash => write!(f, "engine crash"),
            Self::FlagFall => write!(f, "flag fall"),
        }
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
    let mut wtime = config.base_time_ms as i64;
    let mut btime = config.base_time_ms as i64;
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

        // Check legal moves for the side to move
        let mut legal = MoveList::new();
        board.generate_legal_moves(&mut legal);

        if legal.is_empty() {
            let king = Piece::from_kind(PieceKind::King, board.turn);
            let king_sq = board.piece_bb[king.index()].trailing_zeros();
            let in_check = king_sq < 64
                && board.is_square_attacked(king_sq as i32, board.turn.other());

            if in_check {
                let result = if board.turn == Color::White {
                    GameResult::BlackWin
                } else {
                    GameResult::WhiteWin
                };
                return GameOutcome {
                    result,
                    reason: GameEndReason::Checkmate,
                    move_count: ply,
                };
            } else {
                return GameOutcome {
                    result: GameResult::Draw,
                    reason: GameEndReason::Stalemate,
                    move_count: ply,
                };
            }
        }

        // Check draw conditions
        if board.is_draw_by_repetition() || board.halfmove_clock >= 100 {
            let reason = if board.is_draw_by_repetition() {
                GameEndReason::Repetition
            } else {
                GameEndReason::FiftyMoveRule
            };
            return GameOutcome {
                result: GameResult::Draw,
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
        let bestmove = match engine.go(wtime, btime, config.increment_ms, config.increment_ms) {
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
        let clock = if board.turn == Color::White {
            &mut wtime
        } else {
            &mut btime
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
        *clock += config.increment_ms as i64;

        // Handle "(none)" or "0000"
        if bestmove == "(none)" || bestmove == "0000" {
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
