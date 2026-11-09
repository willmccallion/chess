use crate::board::Board;
use crate::magics;
use crate::types::{Bitboard, Centipawns, Color, Move, Piece, PieceKind};

/// Exchange values indexed by `PieceKind` (P, N, B, R, Q, K).
pub const PIECE_VALUES: [Centipawns; 6] = [
    Centipawns::new(100),
    Centipawns::new(320),
    Centipawns::new(330),
    Centipawns::new(500),
    Centipawns::new(900),
    Centipawns::new(20000),
];

#[inline(always)]
fn piece_value(p: Piece) -> i32 {
    p.kind().map_or(0, kind_value)
}

/// The squares from which a `kind` of `side` attacks `sq` through `occupied`.
fn attack_sources(kind: PieceKind, side: Color, sq: usize, occupied: Bitboard) -> Bitboard {
    match kind {
        PieceKind::Pawn if side == Color::White => magics::BLACK_PAWN_ATTACKS[sq],
        PieceKind::Pawn => magics::WHITE_PAWN_ATTACKS[sq],
        PieceKind::Knight => magics::knight_attacks_from(sq),
        PieceKind::Bishop => magics::get_bishop_attacks(sq, occupied),
        PieceKind::Rook => magics::get_rook_attacks(sq, occupied),
        PieceKind::Queen => {
            magics::get_bishop_attacks(sq, occupied) | magics::get_rook_attacks(sq, occupied)
        }
        PieceKind::King => magics::king_attacks_from(sq),
    }
}

/// The least valuable piece of `side` attacking `sq` through `occupied`, and
/// its square.
fn least_valuable_attacker(
    b: &Board,
    sq: usize,
    occupied: Bitboard,
    side: Color,
) -> Option<(Piece, usize)> {
    SEE_ORDER.iter().find_map(|&kind| {
        let piece = Piece::from_kind(kind, side);
        let attackers =
            attack_sources(kind, side, sq, occupied) & b.piece_bb()[piece.index()] & occupied;
        (attackers != 0).then(|| (piece, attackers.trailing_zeros() as usize))
    })
}

pub fn see(b: &Board, mov: Move) -> Centipawns {
    if !mov.is_capture() {
        return Centipawns::ZERO;
    }

    let from_sq = mov.from.index();
    let to_sq = mov.to.index();

    let mut gain = [0; 32];
    let mut gain_idx = 1;

    let mut from_piece = b.piece_on()[from_sq];
    let mut occupied = b.all_pieces();
    let mut current_turn = b.turn();

    let captured_piece = if mov.is_en_passant() {
        Piece::from_kind(PieceKind::Pawn, b.turn().other())
    } else {
        b.piece_on()[to_sq]
    };

    gain[0] = piece_value(captured_piece);

    occupied ^= 1u64 << from_sq;
    if mov.is_en_passant() {
        occupied ^= 1u64 << mov.captured_square(b.turn());
    }

    loop {
        current_turn = current_turn.other();

        if let Some((attacker_piece, attacker_sq)) =
            least_valuable_attacker(b, to_sq, occupied, current_turn)
        {
            occupied ^= 1u64 << attacker_sq;

            if gain_idx >= 32 {
                break;
            }

            gain[gain_idx] = piece_value(from_piece) - gain[gain_idx - 1];
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

    Centipawns::new(gain[0])
}

const fn kind_value(kind: PieceKind) -> i32 {
    PIECE_VALUES[kind as usize].get()
}

/// All pieces of both colors attacking `sq` given the occupancy `occupied`.
fn attackers_to(b: &Board, sq: usize, occupied: Bitboard) -> Bitboard {
    let rooks_queens = b.kind_bb(PieceKind::Rook) | b.kind_bb(PieceKind::Queen);
    let bishops_queens = b.kind_bb(PieceKind::Bishop) | b.kind_bb(PieceKind::Queen);
    (magics::BLACK_PAWN_ATTACKS[sq] & b.piece_bb()[Piece::WP.index()])
        | (magics::WHITE_PAWN_ATTACKS[sq] & b.piece_bb()[Piece::BP.index()])
        | (magics::knight_attacks_from(sq) & b.kind_bb(PieceKind::Knight))
        | (magics::king_attacks_from(sq) & b.kind_bb(PieceKind::King))
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

/// Whether the static exchange on the move's target square gains at least
/// `threshold` for the side to move.
///
/// Handles quiet moves and x-ray attackers; castling and promotions count as
/// an exchange worth zero.
pub fn see_ge(b: &Board, m: Move, threshold: Centipawns) -> bool {
    let threshold = threshold.get();
    let mover_kind = b.piece_on()[m.from.index()].kind();
    let Some(mover_kind) = mover_kind.filter(|_| !m.is_castle() && m.promotion().is_none()) else {
        return 0 >= threshold;
    };
    let to = m.to.index();
    let captured_value = if m.is_en_passant() {
        kind_value(PieceKind::Pawn)
    } else {
        b.piece_on()[to].kind().map_or(0, kind_value)
    };
    let swap = captured_value - threshold;
    if swap < 0 {
        return false;
    }
    let swap = kind_value(mover_kind) - swap;
    if swap <= 0 {
        return true;
    }

    let mut occupied = b.all_pieces() ^ (1u64 << m.from.index()) ^ (1u64 << to);
    if m.is_en_passant() {
        occupied ^= 1u64 << m.captured_square(b.turn());
    }
    wins_exchange(b, to, occupied, swap)
}

/// Plays out the recaptures on `to` after the side to move's move, least
/// valuable attacker first, with `swap` what the side to move would lose if
/// recaptured; whether it still makes its threshold.
fn wins_exchange(b: &Board, to: usize, mut occupied: Bitboard, mut swap: i32) -> bool {
    let sliders = Sliders::of(b);
    let mut side = b.turn();
    let mut attackers = attackers_to(b, to, occupied);
    let mut result = true;
    loop {
        side = side.other();
        attackers &= occupied;
        let side_attackers = attackers & b.pieces_of(side);
        let Some((kind, attacker_bb)) = least_valuable_of(b, side_attackers) else {
            break;
        };
        result = !result;
        if kind == PieceKind::King {
            let opponent_still_attacks = attackers & b.pieces_of(side.other()) != 0;
            return result != opponent_still_attacks;
        }
        swap = kind_value(kind) - swap;
        if swap < i32::from(result) {
            break;
        }
        occupied ^= attacker_bb.isolate_lowest_one();
        attackers |= sliders.revealed_behind(kind, to, occupied);
    }
    result
}

/// The least valuable kind among `attackers`, and the attackers of that kind.
fn least_valuable_of(b: &Board, attackers: Bitboard) -> Option<(PieceKind, Bitboard)> {
    SEE_ORDER
        .iter()
        .map(|&kind| (kind, attackers & b.kind_bb(kind)))
        .find(|&(_, bb)| bb != 0)
}

/// Both sides' sliders, by the lines they move along.
struct Sliders {
    diagonal: Bitboard,
    straight: Bitboard,
}

impl Sliders {
    const fn of(b: &Board) -> Self {
        let queens = b.kind_bb(PieceKind::Queen);
        Self {
            diagonal: b.kind_bb(PieceKind::Bishop) | queens,
            straight: b.kind_bb(PieceKind::Rook) | queens,
        }
    }

    /// The sliders that attack `to` through the square a `kind` just left.
    fn revealed_behind(&self, kind: PieceKind, to: usize, occupied: Bitboard) -> Bitboard {
        let mut revealed = 0;
        if matches!(kind, PieceKind::Pawn | PieceKind::Bishop | PieceKind::Queen) {
            revealed |= magics::get_bishop_attacks(to, occupied) & self.diagonal;
        }
        if matches!(kind, PieceKind::Rook | PieceKind::Queen) {
            revealed |= magics::get_rook_attacks(to, occupied) & self.straight;
        }
        revealed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn see_value_bracket(fen: &str, uci: &str, expected: i32) {
        let expected = Centipawns::new(expected);
        let board = Board::from_fen(fen).unwrap();
        let m = crate::uci_io::parse_uci_move(&board, uci).unwrap();
        assert!(
            see_ge(&board, m, expected),
            "{uci} should gain at least {expected}"
        );
        assert!(
            !see_ge(&board, m, expected + Centipawns::new(1)),
            "{uci} should gain less than {}",
            expected + Centipawns::new(1)
        );
    }

    #[test]
    fn undefended_pawn_capture_wins_a_pawn() {
        see_value_bracket(
            "1k1r4/1pp4p/p7/4p3/8/P5P1/1PP4P/2K1R3 w - - 0 1",
            "e1e5",
            100,
        );
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

    #[test]
    fn see_of_en_passant_sees_the_xray_through_the_captured_pawn() {
        let board = Board::from_fen("3r3k/8/8/3pP3/8/8/8/3R3K w - d6 0 1").unwrap();
        let exd6 = crate::uci_io::parse_uci_move(&board, "e5d6").unwrap();

        let value = see(&board, exd6);

        assert_eq!(
            value,
            Centipawns::new(100),
            "exd6 wins a pawn: Rxd6 Rxd6 loses Black's rook"
        );
    }
}
