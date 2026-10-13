//! Input feature indices: HalfKAv2_hm (piece-square) and FullThreats (attacker-target pairs).

use super::arch::{PSQ_DIMENSIONS, THREAT_DIMENSIONS};
use crate::eval::PositionView;
use crate::magics;
use crate::types::{Bitboard, Color, Piece, PieceKind};
use std::sync::OnceLock;

/// Stockfish's piece code: kind 1..=6, plus 8 for black.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct SfPiece(u8);

const SF_PIECE_CODES: usize = 16;
const COLOR_BIT: u8 = 8;

impl SfPiece {
    pub fn new(piece: Piece) -> Option<Self> {
        let kind = piece.kind()? as u8;
        let color = piece.color()? as u8;
        Some(Self(kind + 1 + COLOR_BIT * color))
    }

    const fn kind_index(self) -> usize {
        (self.0 & 7) as usize - 1
    }

    const fn is_black(self) -> bool {
        self.0 & COLOR_BIT != 0
    }

    const fn flipped_color(self) -> Self {
        Self(self.0 ^ COLOR_BIT)
    }
}

const PS_KING: usize = 10 * 64;
const PS_NB: usize = 11 * 64;

/// Mirror files so the king stands on files e..h.
const fn psq_orientation(king_sq: usize) -> usize {
    if king_sq % 8 < 4 { 7 } else { 0 }
}

const fn king_bucket(king_sq: usize) -> usize {
    let file = king_sq % 8;
    let rank = king_sq / 8;
    4 * (7 - rank) + if file < 4 { file } else { 7 - file }
}

const fn vertical_flip(perspective: Color) -> usize {
    match perspective {
        Color::White => 0,
        Color::Black => 56,
    }
}

/// HalfKAv2_hm feature of `piece` on `sq` seen by `perspective`, whose king is on `king_sq`.
pub fn psq_index(perspective: Color, sq: usize, piece: SfPiece, king_sq: usize) -> usize {
    let flip = vertical_flip(perspective);
    let piece_base = if piece.kind_index() == PieceKind::King as usize {
        PS_KING
    } else {
        let theirs = piece.is_black() != (perspective == Color::Black);
        piece.kind_index() * 128 + usize::from(theirs) * 64
    };
    let index = (sq ^ psq_orientation(king_sq) ^ flip) + piece_base + king_bucket(king_sq ^ flip) * PS_NB;
    debug_assert!(index < PSQ_DIMENSIONS);
    index
}

/// Attack targets per Stockfish piece code: pawn 6, knight 12, bishop 10, rook 10, queen 12, king 8.
const VALID_TARGETS: [usize; 6] = [6, 12, 10, 10, 12, 8];

/// `[attacker kind][target kind]` slot among the attacker's targets; `None` is never a feature.
const TARGET_SLOT: [[Option<usize>; 6]; 6] = [
    [Some(0), Some(1), None, Some(2), None, None],
    [Some(0), Some(1), Some(2), Some(3), Some(4), Some(5)],
    [Some(0), Some(1), Some(2), Some(3), None, Some(4)],
    [Some(0), Some(1), Some(2), Some(3), None, Some(4)],
    [Some(0), Some(1), Some(2), Some(3), Some(4), Some(5)],
    [Some(0), Some(1), Some(2), Some(3), None, None],
];

const ALL_SF_PIECES: [u8; 12] = [1, 2, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14];

/// Squares a piece attacks from `from` on an empty board.
fn pseudo_attacks(piece: SfPiece, from: usize) -> Bitboard {
    match piece.kind_index() {
        0 if piece.is_black() => magics::BLACK_PAWN_ATTACKS[from],
        0 => magics::WHITE_PAWN_ATTACKS[from],
        1 => magics::knight_attacks_from(from),
        2 => magics::get_bishop_attacks(from, 0),
        3 => magics::get_rook_attacks(from, 0),
        4 => magics::get_bishop_attacks(from, 0) | magics::get_rook_attacks(from, 0),
        _ => magics::king_attacks_from(from),
    }
}

struct ThreatTables {
    /// `[attacker][attacked][from < to]`; `THREAT_DIMENSIONS` marks a pair that is not a feature.
    pair_base: [[[u32; 2]; SF_PIECE_CODES]; SF_PIECE_CODES],
    /// `[attacker][from]`
    from_offset: [[u32; 64]; SF_PIECE_CODES],
    /// `[attacker][from][to]`: rank of `to` among the attacked squares.
    target_rank: Vec<[[u8; 64]; 64]>,
    total_features: usize,
}

static THREAT_TABLES: OnceLock<Box<ThreatTables>> = OnceLock::new();

fn threat_tables() -> &'static ThreatTables {
    THREAT_TABLES.get_or_init(build_threat_tables)
}

fn build_threat_tables() -> Box<ThreatTables> {
    let mut tables = Box::new(ThreatTables {
        pair_base: [[[THREAT_DIMENSIONS as u32; 2]; SF_PIECE_CODES]; SF_PIECE_CODES],
        from_offset: [[0; 64]; SF_PIECE_CODES],
        target_rank: vec![[[0; 64]; 64]; SF_PIECE_CODES],
        total_features: 0,
    });

    let mut squares_per_piece = [0usize; SF_PIECE_CODES];
    let mut piece_base = [0usize; SF_PIECE_CODES];
    let mut running = 0;
    for code in ALL_SF_PIECES {
        let piece = SfPiece(code);
        let mut cumulative = 0;
        for from in 0..64 {
            tables.from_offset[code as usize][from] = cumulative as u32;
            let is_pawn = piece.kind_index() == 0;
            if !is_pawn || (8..56).contains(&from) {
                cumulative += pseudo_attacks(piece, from).count_ones() as usize;
            }
        }
        squares_per_piece[code as usize] = cumulative;
        piece_base[code as usize] = running;
        running += VALID_TARGETS[piece.kind_index()] * cumulative;
    }
    tables.total_features = running;

    for attacker_code in ALL_SF_PIECES {
        let attacker = SfPiece(attacker_code);
        for attacked_code in ALL_SF_PIECES {
            let attacked = SfPiece(attacked_code);
            let enemy = attacker.is_black() != attacked.is_black();
            let same_kind = attacker.kind_index() == attacked.kind_index();
            let semi_excluded = same_kind && (enemy || attacker.kind_index() != 0);
            let Some(slot) = TARGET_SLOT[attacker.kind_index()][attacked.kind_index()] else {
                continue;
            };
            let color_offset = usize::from(attacked.is_black()) * (VALID_TARGETS[attacker.kind_index()] / 2);
            let feature = piece_base[attacker_code as usize]
                + (color_offset + slot) * squares_per_piece[attacker_code as usize];
            let entry = &mut tables.pair_base[attacker_code as usize][attacked_code as usize];
            entry[0] = feature as u32;
            if !semi_excluded {
                entry[1] = feature as u32;
            }
        }

        for from in 0..64 {
            let attacks = pseudo_attacks(attacker, from);
            for to in 0..64 {
                let below = if to == 0 { 0 } else { (1u64 << to) - 1 };
                tables.target_rank[attacker_code as usize][from][to] = (below & attacks).count_ones() as u8;
            }
        }
    }
    tables
}

/// Mirror files so the king stands on files a..d.
const fn threat_orientation(king_sq: usize) -> usize {
    if king_sq % 8 < 4 { 0 } else { 7 }
}

/// FullThreats feature for `attacker` on `from` hitting `attacked` on `to`, or `None` if
/// the pair is not an input of the network.
pub fn threat_index(
    perspective: Color,
    attacker: SfPiece,
    from: usize,
    to: usize,
    attacked: SfPiece,
    king_sq: usize,
) -> Option<usize> {
    let tables = threat_tables();
    let orientation = threat_orientation(king_sq) ^ vertical_flip(perspective);
    let from = from ^ orientation;
    let to = to ^ orientation;
    let (attacker, attacked) = match perspective {
        Color::White => (attacker, attacked),
        Color::Black => (attacker.flipped_color(), attacked.flipped_color()),
    };
    let index = tables.pair_base[attacker.0 as usize][attacked.0 as usize][usize::from(from < to)]
        as usize
        + tables.from_offset[attacker.0 as usize][from] as usize
        + tables.target_rank[attacker.0 as usize][from][to] as usize;
    (index < THREAT_DIMENSIONS).then_some(index)
}

fn sf_piece_on(position: PositionView, sq: usize) -> Option<SfPiece> {
    SfPiece::new(position.piece_on[sq])
}

/// Squares attacked by the piece on `from`, given `occupied`.
pub fn attacks_from(piece: SfPiece, from: usize, occupied: Bitboard) -> Bitboard {
    match piece.kind_index() {
        2 => magics::get_bishop_attacks(from, occupied),
        3 => magics::get_rook_attacks(from, occupied),
        4 => magics::get_bishop_attacks(from, occupied) | magics::get_rook_attacks(from, occupied),
        _ => pseudo_attacks(piece, from),
    }
}

pub fn king_square(position: PositionView, color: Color) -> usize {
    position.piece_bb[Piece::from_kind(PieceKind::King, color).index()].trailing_zeros() as usize
}

/// Every active HalfKAv2_hm feature of `perspective`.
pub fn active_psq_features(position: PositionView, perspective: Color, out: &mut Vec<usize>) {
    let king_sq = king_square(position, perspective);
    let mut occupied = position.occupied;
    while occupied != 0 {
        let sq = occupied.trailing_zeros() as usize;
        occupied &= occupied - 1;
        if let Some(piece) = sf_piece_on(position, sq) {
            out.push(psq_index(perspective, sq, piece, king_sq));
        }
    }
}

/// Every active FullThreats feature of `perspective`.
pub fn active_threat_features(position: PositionView, perspective: Color, out: &mut Vec<usize>) {
    let king_sq = king_square(position, perspective);
    let occupied = position.occupied;
    let mut pieces = occupied;
    while pieces != 0 {
        let from = pieces.trailing_zeros() as usize;
        pieces &= pieces - 1;
        let Some(attacker) = sf_piece_on(position, from) else {
            continue;
        };
        let mut targets = attacks_from(attacker, from, occupied) & occupied;
        while targets != 0 {
            let to = targets.trailing_zeros() as usize;
            targets &= targets - 1;
            let Some(attacked) = sf_piece_on(position, to) else {
                continue;
            };
            if let Some(index) = threat_index(perspective, attacker, from, to, attacked, king_sq) {
                out.push(index);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;
    use crate::types::START_FEN;

    #[test]
    fn piece_square_features_are_unique_and_in_range() {
        let board = Board::from_fen(START_FEN).unwrap();
        for perspective in [Color::White, Color::Black] {
            let mut features = Vec::new();
            active_psq_features(board.position_view(), perspective, &mut features);

            let mut sorted = features.clone();
            sorted.sort_unstable();
            sorted.dedup();

            assert_eq!(sorted.len(), 32);
            assert!(features.iter().all(|&f| f < PSQ_DIMENSIONS));
        }
    }

    #[test]
    fn start_position_looks_the_same_from_both_sides() {
        let board = Board::from_fen(START_FEN).unwrap();
        let mut white = (Vec::new(), Vec::new());
        let mut black = (Vec::new(), Vec::new());

        active_psq_features(board.position_view(), Color::White, &mut white.0);
        active_psq_features(board.position_view(), Color::Black, &mut black.0);
        active_threat_features(board.position_view(), Color::White, &mut white.1);
        active_threat_features(board.position_view(), Color::Black, &mut black.1);
        for list in [&mut white.0, &mut white.1, &mut black.0, &mut black.1] {
            list.sort_unstable();
        }

        assert_eq!(white, black);
    }

    #[test]
    fn threat_tables_partition_exactly_the_network_inputs() {
        assert_eq!(threat_tables().total_features, THREAT_DIMENSIONS);
    }
}
