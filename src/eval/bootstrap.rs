//! A small hand-written evaluation that plays the first generation of training games.
//! Its piece-square terms come from simple formulas rather than tuned tables.

use super::{Evaluator, IncrementalState, PositionView};
use crate::types::{Color, Move, Piece, PieceKind};

/// Middlegame and endgame values of each piece kind, pawn first.
const MATERIAL: [(i32, i32); 6] = [
    (90, 120),
    (320, 300),
    (330, 320),
    (480, 540),
    (950, 980),
    (0, 0),
];
/// Game phase each piece kind contributes; the full opening total is [`MAX_PHASE`].
const PHASE_WEIGHT: [i32; 6] = [0, 1, 1, 2, 4, 0];
const MAX_PHASE: i32 = 24;
const BISHOP_PAIR: (i32, i32) = (30, 50);
const TEMPO: i32 = 15;

const KINDS: [PieceKind; 6] = [
    PieceKind::Pawn,
    PieceKind::Knight,
    PieceKind::Bishop,
    PieceKind::Rook,
    PieceKind::Queen,
    PieceKind::King,
];

/// Distance from the four centre squares: 0 in the centre, 3 on the rim.
const fn center_distance(file: i32, rank: i32) -> i32 {
    let file_distance = if file < 4 { 3 - file } else { file - 4 };
    let rank_distance = if rank < 4 { 3 - rank } else { rank - 4 };
    if file_distance > rank_distance {
        file_distance
    } else {
        rank_distance
    }
}

/// Positional (middlegame, endgame) bonus for a piece on `square`, seen from its own side
/// (rank 0 is its back rank).
const fn placement(kind: PieceKind, file: i32, rank: i32) -> (i32, i32) {
    let center = center_distance(file, rank);
    let central_file = file >= 2 && file <= 5;
    match kind {
        PieceKind::Pawn => {
            let advance = rank - 1;
            let center_push = if central_file && rank >= 3 { 10 } else { 0 };
            (4 * advance + center_push, 12 * advance)
        }
        PieceKind::Knight => (30 - 12 * center, 20 - 8 * center),
        PieceKind::Bishop => (15 - 6 * center, 10 - 4 * center),
        PieceKind::Rook => {
            let seventh = if rank == 6 { 20 } else { 0 };
            let central = if file >= 3 && file <= 4 { 5 } else { 0 };
            (seventh + central, seventh / 2)
        }
        PieceKind::Queen => (5 - 3 * center, 15 - 5 * center),
        PieceKind::King => {
            let shelter = if rank == 0 && (file <= 2 || file >= 6) {
                20
            } else {
                0
            };
            (shelter - 25 * rank, 30 - 12 * center)
        }
    }
}

/// Middlegame and endgame totals for `color`, and the phase its pieces contribute.
fn side_score(position: PositionView, color: Color) -> (i32, i32, i32) {
    let (mut middlegame, mut endgame, mut phase) = (0, 0, 0);
    for (index, &kind) in KINDS.iter().enumerate() {
        let mut squares = position.piece_bb[Piece::from_kind(kind, color).index()];
        let count = squares.count_ones() as i32;
        middlegame += count * MATERIAL[index].0;
        endgame += count * MATERIAL[index].1;
        phase += count * PHASE_WEIGHT[index];
        while squares != 0 {
            let square = squares.trailing_zeros() as i32;
            squares &= squares - 1;
            let rank = if color == Color::White {
                square / 8
            } else {
                7 - square / 8
            };
            let (mg, eg) = placement(kind, square % 8, rank);
            middlegame += mg;
            endgame += eg;
        }
        if kind == PieceKind::Bishop && count >= 2 {
            middlegame += BISHOP_PAIR.0;
            endgame += BISHOP_PAIR.1;
        }
    }
    (middlegame, endgame, phase)
}

/// The evaluation; it needs no state between moves.
pub struct Bootstrap;

/// The bootstrap evaluation keeps nothing between moves.
#[derive(Clone, Copy, Default)]
pub struct NoState;

impl IncrementalState for NoState {
    fn push_move(&mut self, _before: PositionView, _m: Move) {}
    fn pop_move(&mut self) {}
    fn reset(&mut self) {}
}

impl Evaluator for Bootstrap {
    type State = NoState;

    fn new_state(&self) -> NoState {
        NoState
    }

    fn evaluate(&self, _state: &mut NoState, position: PositionView) -> i32 {
        let (white_mg, white_eg, white_phase) = side_score(position, Color::White);
        let (black_mg, black_eg, black_phase) = side_score(position, Color::Black);
        let phase = (white_phase + black_phase).min(MAX_PHASE);
        let white_relative = ((white_mg - black_mg) * phase
            + (white_eg - black_eg) * (MAX_PHASE - phase))
            / MAX_PHASE;
        let side_relative = if position.turn == Color::White {
            white_relative
        } else {
            -white_relative
        };
        side_relative + TEMPO
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;
    use crate::types::START_FEN;

    fn eval(fen: &str) -> i32 {
        let board = Board::from_fen(fen).unwrap();
        Bootstrap.evaluate(&mut NoState, board.position_view())
    }

    #[test]
    fn the_start_position_is_worth_only_the_tempo() {
        assert_eq!(eval(START_FEN), TEMPO);
    }

    #[test]
    fn mirrored_positions_score_the_same_for_the_side_to_move() {
        let pairs = [
            (
                "r1bqkb1r/pppp1ppp/2n2n2/4p3/2B1P3/5N2/PPPP1PPP/RNBQK2R w KQkq - 4 4",
                "rnbqk2r/pppp1ppp/5n2/2b1p3/4P3/2N2N2/PPPP1PPP/R1BQKB1R b KQkq - 4 4",
            ),
            (
                "8/5k2/8/3K4/8/8/1P6/8 b - - 0 1",
                "8/1p6/8/8/3k4/8/5K2/8 w - - 0 1",
            ),
        ];
        for (fen, mirrored) in pairs {
            assert_eq!(eval(fen), eval(mirrored), "{fen}");
        }
    }

    #[test]
    fn extra_material_is_worth_about_its_value() {
        let up_a_knight = eval("rnbqkb1r/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");

        assert!((250..=400).contains(&up_a_knight), "{up_a_knight}");
    }

    #[test]
    fn centralised_knights_beat_knights_on_the_rim() {
        let central = eval("4k3/8/8/8/3N4/8/8/4K3 w - - 0 1");
        let rim = eval("4k3/8/8/8/N7/8/8/4K3 w - - 0 1");

        assert!(central > rim);
    }
}
