use crate::board::Board;
use crate::magics;
use crate::types::{Move, Piece, PieceKind};

const PIECE_VALUES: [i32; 6] = [100, 320, 330, 500, 900, 20000]; // P, N, B, R, Q, K

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
