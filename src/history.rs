//! Move-ordering statistics updated from search results.

use crate::types::{Color, Move, Piece, PieceKind};

pub const HISTORY_MAX: i32 = 16_384;
const MAX_BONUS: i32 = 1_500;

/// Scores indexed by `[piece][to]`; one table per previous (piece, to) pair.
pub type PieceToHistory = [[i16; 64]; 13];

const CONTINUATION_TABLES: usize = 13 * 64;

pub struct Histories {
    butterfly: [[[i16; 64]; 64]; 2],
    continuation: Vec<PieceToHistory>,
    capture: [[[i16; 6]; 64]; 13],
}

/// History bonus for a move that caused (or failed to cause) a cutoff at `depth`.
pub fn stat_bonus(depth: i32) -> i32 {
    (16 * depth * depth + 32 * depth + 16).min(MAX_BONUS)
}

/// Moves the entry toward ±HISTORY_MAX, shrinking updates as it saturates.
#[inline(always)]
fn apply_gravity(entry: &mut i16, bonus: i32) {
    let value = i32::from(*entry);
    *entry = (value + bonus - value * bonus.abs() / HISTORY_MAX) as i16;
}

/// Identifies the continuation-history table that follows `piece` moving to `to`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ContinuationKey(usize);

impl ContinuationKey {
    pub fn new(piece: Piece, to: u8) -> Self {
        Self(piece.index() * 64 + to as usize)
    }
}

/// The victim of a capture, or `None` for a quiet promotion.
pub fn captured_kind(board_piece_on_to: Piece, m: Move) -> Option<PieceKind> {
    if m.en_passant {
        Some(PieceKind::Pawn)
    } else {
        board_piece_on_to.kind()
    }
}

impl Histories {
    pub fn new() -> Self {
        Self {
            butterfly: [[[0; 64]; 64]; 2],
            continuation: vec![[[0; 64]; 13]; CONTINUATION_TABLES],
            capture: [[[0; 6]; 64]; 13],
        }
    }

    #[inline(always)]
    pub fn butterfly(&self, color: Color, m: Move) -> i32 {
        i32::from(self.butterfly[color as usize][m.from as usize][m.to as usize])
    }

    #[inline(always)]
    pub fn continuation(&self, key: ContinuationKey, piece: Piece, to: u8) -> i32 {
        i32::from(self.continuation[key.0][piece.index()][to as usize])
    }

    #[inline(always)]
    pub fn capture(&self, piece: Piece, to: u8, victim: PieceKind) -> i32 {
        i32::from(self.capture[piece.index()][to as usize][victim as usize])
    }

    pub fn update_quiet(
        &mut self,
        color: Color,
        m: Move,
        piece: Piece,
        continuations: &[Option<ContinuationKey>],
        bonus: i32,
    ) {
        apply_gravity(
            &mut self.butterfly[color as usize][m.from as usize][m.to as usize],
            bonus,
        );
        for key in continuations.iter().flatten() {
            apply_gravity(
                &mut self.continuation[key.0][piece.index()][m.to as usize],
                bonus,
            );
        }
    }

    pub fn update_capture(&mut self, piece: Piece, to: u8, victim: PieceKind, bonus: i32) {
        apply_gravity(
            &mut self.capture[piece.index()][to as usize][victim as usize],
            bonus,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gravity_keeps_entries_within_bounds() {
        let mut entry = 0i16;
        for _ in 0..1000 {
            apply_gravity(&mut entry, MAX_BONUS);
        }
        assert!(i32::from(entry) <= HISTORY_MAX);
        for _ in 0..1000 {
            apply_gravity(&mut entry, -MAX_BONUS);
        }
        assert!(i32::from(entry) >= -HISTORY_MAX);
    }

    #[test]
    fn bonus_grows_with_depth_and_is_capped() {
        assert!(stat_bonus(2) > stat_bonus(1));
        assert_eq!(stat_bonus(50), MAX_BONUS);
    }

    #[test]
    fn quiet_update_touches_butterfly_and_continuation_tables() {
        let mut histories = Histories::new();
        let m = Move::quiet(12, 28);
        let key = ContinuationKey::new(Piece::BN, 45);

        histories.update_quiet(Color::White, m, Piece::WP, &[Some(key), None], 400);

        assert_eq!(histories.butterfly(Color::White, m), 400);
        assert_eq!(histories.continuation(key, Piece::WP, 28), 400);
        assert_eq!(histories.butterfly(Color::Black, m), 0);
    }
}
