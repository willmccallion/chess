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

const CORRECTION_ENTRIES: usize = 16_384;
const CORRECTION_GRAIN: i32 = 256;
const CORRECTION_WEIGHT_SCALE: i32 = 256;
const CORRECTION_MAX_WEIGHT: i32 = 16;
const CORRECTION_LIMIT_CP: i32 = 128;

/// Learned static-eval error per pawn structure, in 1/256 centipawn units.
pub struct CorrectionHistory {
    entries: Vec<[i32; CORRECTION_ENTRIES]>,
}

impl CorrectionHistory {
    pub fn new() -> Self {
        Self {
            entries: vec![[0; CORRECTION_ENTRIES]; 2],
        }
    }

    fn slot(pawn_key: u64) -> usize {
        (pawn_key as usize) % CORRECTION_ENTRIES
    }

    /// Centipawns to add to the static eval of a position with this pawn structure.
    pub fn correction(&self, color: Color, pawn_key: u64) -> i32 {
        self.entries[color as usize][Self::slot(pawn_key)] / CORRECTION_GRAIN
    }

    /// Moves the stored correction toward `error` (search score minus corrected eval),
    /// trusting deeper searches more.
    pub fn update(&mut self, color: Color, pawn_key: u64, error: i32, depth: i32) {
        let weight = (depth + 1).min(CORRECTION_MAX_WEIGHT);
        let limit = CORRECTION_LIMIT_CP * CORRECTION_GRAIN;
        let entry = &mut self.entries[color as usize][Self::slot(pawn_key)];
        let target = (error * CORRECTION_GRAIN).clamp(-limit, limit);
        let blended = (*entry * (CORRECTION_WEIGHT_SCALE - weight) + target * weight)
            / CORRECTION_WEIGHT_SCALE;
        *entry = blended.clamp(-limit, limit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correction_moves_toward_observed_error_and_stays_bounded() {
        let mut correction = CorrectionHistory::new();
        for _ in 0..200 {
            correction.update(Color::White, 99, 80, 10);
        }
        let learned = correction.correction(Color::White, 99);
        assert!((70..=80).contains(&learned), "{learned}");
        assert_eq!(correction.correction(Color::Black, 99), 0);

        for _ in 0..200 {
            correction.update(Color::White, 99, 10_000, 20);
        }
        let saturated = correction.correction(Color::White, 99);
        assert!((CORRECTION_LIMIT_CP - 2..=CORRECTION_LIMIT_CP).contains(&saturated), "{saturated}");
    }

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
