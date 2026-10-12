//! Legality of pseudo-legal moves from pins and checkers, without making the move.

use crate::board::Board;
use crate::magics;
use crate::types::{Bitboard, Color, Move, Piece, PieceKind};
use std::sync::OnceLock;

struct LineTables {
    /// Squares strictly between two aligned squares.
    between: [[Bitboard; 64]; 64],
    /// The full rank, file or diagonal through two aligned squares.
    line: [[Bitboard; 64]; 64],
}

static LINE_TABLES: OnceLock<Box<LineTables>> = OnceLock::new();

fn line_tables() -> &'static LineTables {
    LINE_TABLES.get_or_init(|| {
        let mut tables = Box::new(LineTables {
            between: [[0; 64]; 64],
            line: [[0; 64]; 64],
        });
        for a in 0..64 {
            for b in 0..64 {
                if a == b {
                    continue;
                }
                let (a_bb, b_bb) = (1u64 << a, 1u64 << b);
                if magics::get_rook_attacks(a, 0) & b_bb != 0 {
                    tables.between[a][b] =
                        magics::get_rook_attacks(a, b_bb) & magics::get_rook_attacks(b, a_bb);
                    tables.line[a][b] =
                        (magics::get_rook_attacks(a, 0) & magics::get_rook_attacks(b, 0)) | a_bb | b_bb;
                } else if magics::get_bishop_attacks(a, 0) & b_bb != 0 {
                    tables.between[a][b] =
                        magics::get_bishop_attacks(a, b_bb) & magics::get_bishop_attacks(b, a_bb);
                    tables.line[a][b] = (magics::get_bishop_attacks(a, 0)
                        & magics::get_bishop_attacks(b, 0))
                        | a_bb
                        | b_bb;
                }
            }
        }
        tables
    })
}

/// The full rank, file or diagonal through two squares; empty when they are not aligned.
pub fn line_through(a: usize, b: usize) -> Bitboard {
    line_tables().line[a][b]
}

/// Pins and checks against the side to move, computed once per position.
#[derive(Copy, Clone, Debug)]
pub struct LegalityInfo {
    king_sq: usize,
    checkers: Bitboard,
    pinned: Bitboard,
}

impl LegalityInfo {
    pub fn in_check(&self) -> bool {
        self.checkers != 0
    }
}

fn pieces(board: &Board, kind: PieceKind, color: Color) -> Bitboard {
    board.piece_bb[Piece::from_kind(kind, color).index()]
}

fn color_pieces(board: &Board, color: Color) -> Bitboard {
    match color {
        Color::White => board.w_pieces,
        Color::Black => board.b_pieces,
    }
}

/// Pieces of `by` attacking `sq` when the board occupancy is `occupied`, ignoring
/// any of their pieces in `removed` (e.g. one just captured).
fn attackers_of(
    board: &Board,
    sq: usize,
    by: Color,
    occupied: Bitboard,
    removed: Bitboard,
) -> Bitboard {
    let pawn_attacks = match by {
        Color::White => magics::BLACK_PAWN_ATTACKS[sq],
        Color::Black => magics::WHITE_PAWN_ATTACKS[sq],
    };
    let queens = pieces(board, PieceKind::Queen, by);
    let rook_like = pieces(board, PieceKind::Rook, by) | queens;
    let bishop_like = pieces(board, PieceKind::Bishop, by) | queens;
    let attackers = (pawn_attacks & pieces(board, PieceKind::Pawn, by))
        | (magics::knight_attacks_from(sq) & pieces(board, PieceKind::Knight, by))
        | (magics::king_attacks_from(sq) & pieces(board, PieceKind::King, by))
        | (magics::get_rook_attacks(sq, occupied) & rook_like)
        | (magics::get_bishop_attacks(sq, occupied) & bishop_like);
    attackers & !removed
}

impl Board {
    /// `None` when the side to move has no king (only reachable from a malformed position).
    pub fn legality_info(&self) -> Option<LegalityInfo> {
        let us = self.turn;
        let them = us.other();
        let king_bb = pieces(self, PieceKind::King, us);
        if king_bb == 0 {
            return None;
        }
        let king_sq = king_bb.trailing_zeros() as usize;
        let checkers = attackers_of(self, king_sq, them, self.all_pieces, 0);

        let queens = pieces(self, PieceKind::Queen, them);
        let snipers = (magics::get_rook_attacks(king_sq, 0)
            & (pieces(self, PieceKind::Rook, them) | queens))
            | (magics::get_bishop_attacks(king_sq, 0)
                & (pieces(self, PieceKind::Bishop, them) | queens));
        let tables = line_tables();
        let ours = color_pieces(self, us);
        let mut pinned = 0;
        let mut remaining = snipers;
        while remaining != 0 {
            let sniper = remaining.trailing_zeros() as usize;
            remaining &= remaining - 1;
            let blockers = tables.between[king_sq][sniper] & self.all_pieces;
            if blockers.count_ones() == 1 && blockers & ours != 0 {
                pinned |= blockers;
            }
        }

        Some(LegalityInfo {
            king_sq,
            checkers,
            pinned,
        })
    }

    /// Whether a pseudo-legal move of the side to move leaves its king safe.
    pub fn is_legal(&self, m: Move, info: &LegalityInfo) -> bool {
        let from = m.from as usize;
        let to = m.to as usize;
        let them = self.turn.other();

        if from == info.king_sq {
            if m.castle {
                return true;
            }
            let occupied = self.all_pieces ^ (1u64 << from);
            return attackers_of(self, to, them, occupied, 1u64 << to) == 0;
        }

        if m.en_passant {
            let captured_sq = if self.turn == Color::White { to - 8 } else { to + 8 };
            let captured = 1u64 << captured_sq;
            let occupied = (self.all_pieces ^ (1u64 << from) ^ captured) | (1u64 << to);
            return attackers_of(self, info.king_sq, them, occupied, captured) == 0;
        }

        let tables = line_tables();
        if info.pinned & (1u64 << from) != 0 && tables.line[info.king_sq][from] & (1u64 << to) == 0 {
            return false;
        }

        match info.checkers.count_ones() {
            0 => true,
            1 => {
                let checker_sq = info.checkers.trailing_zeros() as usize;
                let resolves = info.checkers | tables.between[info.king_sq][checker_sq];
                resolves & (1u64 << to) != 0
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::board::Board;
    use crate::types::MoveList;

    /// Legality by make/unmake, the reference the bitboard test must agree with.
    fn legal_by_making(board: &mut Board, m: crate::types::Move) -> bool {
        let mover = board.turn;
        let undo = board.make_move(m);
        let king = board.king_square(mover) as i32;
        let legal = !board.is_square_attacked(king, board.turn);
        board.unmake_move(m, undo);
        legal
    }

    fn assert_agrees_with_make_unmake(board: &mut Board, depth: usize) {
        let mut moves = MoveList::new();
        board.generate_pseudo_legal_moves(&mut moves);
        let info = board.legality_info().unwrap();
        for &m in moves.as_slice() {
            let expected = legal_by_making(board, m);
            assert_eq!(board.is_legal(m, &info), expected, "{} {m:?}", board.to_fen());
            if expected && depth > 1 {
                let undo = board.make_move(m);
                assert_agrees_with_make_unmake(board, depth - 1);
                board.unmake_move(m, undo);
            }
        }
    }

    #[test]
    fn bitboard_legality_matches_make_unmake() {
        let fens = [
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
            "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
            "8/8/8/K2pP2q/8/8/8/7k w - d6 0 1",
        ];
        for fen in fens {
            let mut board = Board::from_fen(fen).unwrap();
            assert_agrees_with_make_unmake(&mut board, 3);
        }
    }
}
