use crate::board::Board;
use crate::magics;
use crate::types::{Bitboard, Color, Move, Piece, PieceKind};

/// Exchange values indexed by `PieceKind` (P, N, B, R, Q, K).
pub const PIECE_VALUES: [i32; 6] = [100, 320, 330, 500, 900, 20000];

#[inline(always)]
fn val(p: Piece) -> i32 {
    if let Some(kind) = p.kind() {
        PIECE_VALUES[kind as usize]
    } else {
        0
    }
}

fn get_attackers(
    b: &Board,
    sq: usize,
    occupied: u64,
    side: crate::types::Color,
) -> (u64, Option<(Piece, usize)>) {
    let mut attackers = 0u64;

    // Check for attackers in increasing order of value (P, N, B, R, Q, K)
    // to find the least valuable attacker (LVA).

    let pawn_kind = Piece::from_kind(PieceKind::Pawn, side);
    let pawn_attacks = if side == crate::types::Color::White {
        magics::BLACK_PAWN_ATTACKS[sq]
    } else {
        magics::WHITE_PAWN_ATTACKS[sq]
    };
    let pawns = b.piece_bb[pawn_kind.index()] & occupied;
    let mut current_attackers = pawn_attacks & pawns;
    if current_attackers != 0 {
        attackers |= current_attackers;
        return (
            attackers,
            Some((pawn_kind, current_attackers.trailing_zeros() as usize)),
        );
    }

    let knight_kind = Piece::from_kind(PieceKind::Knight, side);
    let knights = b.piece_bb[knight_kind.index()] & occupied;
    current_attackers = magics::knight_attacks_from(sq) & knights;
    if current_attackers != 0 {
        attackers |= current_attackers;
        return (
            attackers,
            Some((knight_kind, current_attackers.trailing_zeros() as usize)),
        );
    }

    let bishop_kind = Piece::from_kind(PieceKind::Bishop, side);
    let bishop_attacks = magics::get_bishop_attacks(sq, occupied);
    let bishops = b.piece_bb[bishop_kind.index()] & occupied;
    current_attackers = bishop_attacks & bishops;
    if current_attackers != 0 {
        attackers |= current_attackers;
        return (
            attackers,
            Some((bishop_kind, current_attackers.trailing_zeros() as usize)),
        );
    }

    let rook_kind = Piece::from_kind(PieceKind::Rook, side);
    let rook_attacks = magics::get_rook_attacks(sq, occupied);
    let rooks = b.piece_bb[rook_kind.index()] & occupied;
    current_attackers = rook_attacks & rooks;
    if current_attackers != 0 {
        attackers |= current_attackers;
        return (
            attackers,
            Some((rook_kind, current_attackers.trailing_zeros() as usize)),
        );
    }

    let queen_kind = Piece::from_kind(PieceKind::Queen, side);
    let queens = b.piece_bb[queen_kind.index()] & occupied;
    current_attackers = (bishop_attacks | rook_attacks) & queens;
    if current_attackers != 0 {
        attackers |= current_attackers;
        return (
            attackers,
            Some((queen_kind, current_attackers.trailing_zeros() as usize)),
        );
    }

    let king_kind = Piece::from_kind(PieceKind::King, side);
    let kings = b.piece_bb[king_kind.index()] & occupied;
    current_attackers = magics::king_attacks_from(sq) & kings;
    if current_attackers != 0 {
        attackers |= current_attackers;
        return (
            attackers,
            Some((king_kind, current_attackers.trailing_zeros() as usize)),
        );
    }

    (attackers, None)
}

pub fn see(b: &Board, mov: Move) -> i32 {
    if !mov.capture {
        return 0;
    }

    let from_sq = mov.from as usize;
    let to_sq = mov.to as usize;

    let mut gain = [0; 32];
    let mut gain_idx = 1;

    let mut from_piece = b.piece_on[from_sq];
    let mut occupied = b.all_pieces;
    let mut current_turn = b.turn;

    let captured_piece = if mov.en_passant {
        Piece::from_kind(PieceKind::Pawn, b.turn.other())
    } else {
        b.piece_on[to_sq]
    };

    gain[0] = val(captured_piece);

    occupied ^= 1u64 << from_sq;

    loop {
        current_turn = current_turn.other();

        let (_, lva) = get_attackers(b, to_sq, occupied, current_turn);

        if let Some((attacker_piece, attacker_sq)) = lva {
            occupied ^= 1u64 << attacker_sq;

            if gain_idx >= 32 {
                break;
            }

            gain[gain_idx] = val(from_piece) - gain[gain_idx - 1];
            gain_idx += 1;

            from_piece = attacker_piece;
        } else {
            break;
        }
    }

    while gain_idx > 1 {
        gain_idx -= 1;
        gain[gain_idx - 1] = -(-gain[gain_idx - 1]).max(gain[gain_idx]);
    }

    gain[0]
}

fn kind_value(kind: PieceKind) -> i32 {
    PIECE_VALUES[kind as usize]
}

fn pieces_of(b: &Board, kind: PieceKind) -> Bitboard {
    b.piece_bb[Piece::from_kind(kind, Color::White).index()]
        | b.piece_bb[Piece::from_kind(kind, Color::Black).index()]
}

fn color_pieces(b: &Board, color: Color) -> Bitboard {
    match color {
        Color::White => b.w_pieces,
        Color::Black => b.b_pieces,
    }
}

/// All pieces of both colors attacking `sq` given the occupancy `occupied`.
fn attackers_to(b: &Board, sq: usize, occupied: Bitboard) -> Bitboard {
    let rooks_queens = pieces_of(b, PieceKind::Rook) | pieces_of(b, PieceKind::Queen);
    let bishops_queens = pieces_of(b, PieceKind::Bishop) | pieces_of(b, PieceKind::Queen);
    (magics::BLACK_PAWN_ATTACKS[sq] & b.piece_bb[Piece::WP.index()])
        | (magics::WHITE_PAWN_ATTACKS[sq] & b.piece_bb[Piece::BP.index()])
        | (magics::knight_attacks_from(sq) & pieces_of(b, PieceKind::Knight))
        | (magics::king_attacks_from(sq) & pieces_of(b, PieceKind::King))
        | (magics::get_rook_attacks(sq, occupied) & rooks_queens)
        | (magics::get_bishop_attacks(sq, occupied) & bishops_queens)
}

const SEE_ORDER: [PieceKind; 6] = [
    PieceKind::Pawn,
    PieceKind::Knight,
    PieceKind::Bishop,
    PieceKind::Rook,
    PieceKind::Queen,
    PieceKind::King,
];

/// Whether the static exchange on the move's target square gains at least `threshold`
/// for the side to move. Handles quiet moves and x-ray attackers; castling and
/// promotions are treated as an exchange worth zero.
pub fn see_ge(b: &Board, m: Move, threshold: i32) -> bool {
    if m.castle || m.promotion.is_some() {
        return 0 >= threshold;
    }
    let from = m.from as usize;
    let to = m.to as usize;
    let Some(mover_kind) = b.piece_on[from].kind() else {
        return 0 >= threshold;
    };

    let captured_value = if m.en_passant {
        kind_value(PieceKind::Pawn)
    } else {
        b.piece_on[to].kind().map_or(0, kind_value)
    };
    let mut swap = captured_value - threshold;
    if swap < 0 {
        return false;
    }
    swap = kind_value(mover_kind) - swap;
    if swap <= 0 {
        return true;
    }

    let mut occupied = b.all_pieces ^ (1u64 << from) ^ (1u64 << to);
    if m.en_passant {
        let captured_sq = if b.turn == Color::White { to - 8 } else { to + 8 };
        occupied ^= 1u64 << captured_sq;
    }
    let rooks_queens = pieces_of(b, PieceKind::Rook) | pieces_of(b, PieceKind::Queen);
    let bishops_queens = pieces_of(b, PieceKind::Bishop) | pieces_of(b, PieceKind::Queen);

    let mut side = b.turn;
    let mut attackers = attackers_to(b, to, occupied);
    let mut result = true;

    loop {
        side = side.other();
        attackers &= occupied;
        let side_attackers = attackers & color_pieces(b, side);
        if side_attackers == 0 {
            break;
        }
        result = !result;

        let Some((kind, attacker_bb)) = SEE_ORDER
            .iter()
            .map(|&kind| (kind, side_attackers & pieces_of(b, kind)))
            .find(|&(_, bb)| bb != 0)
        else {
            break;
        };

        if kind == PieceKind::King {
            let opponent_still_attacks = attackers & color_pieces(b, side.other()) != 0;
            return if opponent_still_attacks { !result } else { result };
        }

        swap = kind_value(kind) - swap;
        if swap < i32::from(result) {
            break;
        }
        occupied ^= attacker_bb & attacker_bb.wrapping_neg();

        if matches!(kind, PieceKind::Pawn | PieceKind::Bishop | PieceKind::Queen) {
            attackers |= magics::get_bishop_attacks(to, occupied) & bishops_queens;
        }
        if matches!(kind, PieceKind::Rook | PieceKind::Queen) {
            attackers |= magics::get_rook_attacks(to, occupied) & rooks_queens;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn see_value_bracket(fen: &str, uci: &str, expected: i32) {
        let mut board = Board::from_fen(fen).unwrap();
        let m = crate::uci_io::parse_uci_move(&mut board, uci).unwrap();
        assert!(see_ge(&board, m, expected), "{uci} should gain at least {expected}");
        assert!(!see_ge(&board, m, expected + 1), "{uci} should gain less than {}", expected + 1);
    }

    #[test]
    fn undefended_pawn_capture_wins_a_pawn() {
        see_value_bracket("1k1r4/1pp4p/p7/4p3/8/P5P1/1PP4P/2K1R3 w - - 0 1", "e1e5", 100);
    }

    #[test]
    fn queen_takes_defended_pawn_loses_queen_for_pawn() {
        see_value_bracket("4k3/8/2p5/3p4/8/8/8/3QK3 w - - 0 1", "d1d5", -800);
    }

    #[test]
    fn quiet_move_onto_attacked_square_loses_the_piece() {
        see_value_bracket("k7/8/8/3p4/8/8/8/4RK2 w - - 0 1", "e1e4", -500);
    }

    #[test]
    fn xray_rook_behind_recaptures() {
        see_value_bracket("4k3/3r4/8/3p4/8/8/3R4/3RK3 w - - 0 1", "d2d5", 100);
    }

    #[test]
    fn quiet_move_to_safe_square_is_even() {
        see_value_bracket("4k3/8/8/8/8/8/8/R3K3 w - - 0 1", "a1a5", 0);
    }
}
