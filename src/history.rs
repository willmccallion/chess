//! Move-ordering statistics updated from search results.

use crate::search_params::SearchParams;
use crate::types::{Color, Move, Piece, PieceKind, Square};

pub const HISTORY_MAX: i32 = 16_384;

/// Scores indexed by `[piece][to]`; one table per previous (piece, to) pair.
pub type PieceToHistory = [[i16; 64]; 13];

const CONTINUATION_TABLES: usize = 13 * 64;

pub struct Histories {
    butterfly: [[[i16; 64]; 64]; 2],
    continuation: Vec<PieceToHistory>,
    capture: [[[i16; 6]; 64]; 13],
}

/// History bonus for a move that caused (or failed to cause) a cutoff at `depth`.
pub fn stat_bonus(depth: i32, params: &SearchParams) -> i32 {
    let bonus = params.history_bonus_quadratic * depth * depth
        + params.history_bonus_linear * depth
        + params.history_bonus_constant;
    bonus.clamp(0, params.history_bonus_max)
}

/// Moves the entry toward ±`HISTORY_MAX`, shrinking updates as it saturates.
#[inline(always)]
fn apply_gravity(entry: &mut i16, bonus: i32) {
    let value = i32::from(*entry);
    *entry = (value + bonus - value * bonus.abs() / HISTORY_MAX) as i16;
}

/// Identifies the continuation-history table that follows `piece` moving to `to`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ContinuationKey(usize);

impl ContinuationKey {
    pub const fn new(piece: Piece, to: Square) -> Self {
        Self(piece.index() * 64 + to.index())
    }
}

/// The victim of a capture, or `None` for a quiet promotion.
pub const fn captured_kind(board_piece_on_to: Piece, m: Move) -> Option<PieceKind> {
    if m.is_en_passant() {
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
        i32::from(self.butterfly[color as usize][m.from.index()][m.to.index()])
    }

    #[inline(always)]
    pub fn continuation(&self, key: ContinuationKey, piece: Piece, to: Square) -> i32 {
        i32::from(self.continuation[key.0][piece.index()][to.index()])
    }

    #[inline(always)]
    pub fn capture(&self, piece: Piece, to: Square, victim: PieceKind) -> i32 {
        i32::from(self.capture[piece.index()][to.index()][victim as usize])
    }

    /// A quiet move's history: its butterfly entry and its continuation
    /// entries after the moves `continuations` name.
    #[inline(always)]
    pub fn quiet_history(
        &self,
        color: Color,
        piece: Piece,
        m: Move,
        continuations: &[Option<ContinuationKey>],
    ) -> i32 {
        let continuation: i32 = continuations
            .iter()
            .flatten()
            .map(|&key| self.continuation(key, piece, m.to))
            .sum();
        self.butterfly(color, m) + continuation
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
            &mut self.butterfly[color as usize][m.from.index()][m.to.index()],
            bonus,
        );
        for key in continuations.iter().flatten() {
            apply_gravity(
                &mut self.continuation[key.0][piece.index()][m.to.index()],
                bonus,
            );
        }
    }

    pub fn update_capture(&mut self, piece: Piece, to: Square, victim: PieceKind, bonus: i32) {
        apply_gravity(
            &mut self.capture[piece.index()][to.index()][victim as usize],
            bonus,
        );
    }
}

impl Default for Histories {
    fn default() -> Self {
        Self::new()
    }
}

const CORRECTION_BITS: u32 = 14;
const CORRECTION_ENTRIES: usize = 1 << CORRECTION_BITS;
const CORRECTION_GRAIN: i32 = 256;
const CORRECTION_WEIGHT_SCALE: i32 = 256;
/// The largest correction, in search units.
const CORRECTION_LIMIT: i32 = 128;

/// Learned static-eval error per pawn structure, in 1/256 of the search's
/// score unit (3/4 centipawn), which is what the search feeds it.
pub struct CorrectionHistory {
    /// White's entries, then Black's.
    entries: Box<[i32]>,
}

impl CorrectionHistory {
    pub fn new() -> Self {
        Self {
            entries: vec![0; 2 * CORRECTION_ENTRIES].into_boxed_slice(),
        }
    }

    /// The top bits, since the key's low bits ignore pawns on the far ranks.
    const fn slot(pawn_key: u64) -> usize {
        (pawn_key >> (64 - CORRECTION_BITS)) as usize
    }

    const fn index(color: Color, pawn_key: u64) -> usize {
        color as usize * CORRECTION_ENTRIES + Self::slot(pawn_key)
    }

    /// Search units to add to the static eval of a position with this pawn structure.
    pub fn correction(&self, color: Color, pawn_key: u64) -> i32 {
        self.entries[Self::index(color, pawn_key)] / CORRECTION_GRAIN
    }

    /// Moves the stored correction toward `error` (search score minus corrected eval),
    /// trusting deeper searches more, up to `max_weight` (out of 256).
    pub fn update(&mut self, color: Color, pawn_key: u64, error: i32, depth: i32, max_weight: i32) {
        let weight = (depth + 1).min(max_weight);
        let limit = CORRECTION_LIMIT * CORRECTION_GRAIN;
        let entry = &mut self.entries[Self::index(color, pawn_key)];
        let target = (error * CORRECTION_GRAIN).clamp(-limit, limit);
        let blended = (*entry * (CORRECTION_WEIGHT_SCALE - weight) + target * weight)
            / CORRECTION_WEIGHT_SCALE;
        *entry = blended.clamp(-limit, limit);
    }
}

impl Default for CorrectionHistory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correction_moves_toward_observed_error_and_stays_bounded() {
        let mut correction = CorrectionHistory::new();
        for _ in 0..200 {
            correction.update(Color::White, 99, 80, 10, 16);
        }
        let learned = correction.correction(Color::White, 99);
        assert!((70..=80).contains(&learned), "{learned}");
        assert_eq!(correction.correction(Color::Black, 99), 0);

        for _ in 0..200 {
            correction.update(Color::White, 99, 10_000, 20, 16);
        }
        let saturated = correction.correction(Color::White, 99);
        assert!(
            (CORRECTION_LIMIT - 2..=CORRECTION_LIMIT).contains(&saturated),
            "{saturated}"
        );
    }

    #[test]
    fn every_single_pawn_change_moves_to_a_different_correction_slot() {
        let mut board = crate::board::Board::from_fen(crate::types::START_FEN).unwrap();
        let base = CorrectionHistory::slot(board.pawn_key());
        for piece in [Piece::WP, Piece::BP] {
            for sq in 8..56 {
                board.piece_bb[piece.index()] ^= 1u64 << sq;
                let slot = CorrectionHistory::slot(board.pawn_key());
                board.piece_bb[piece.index()] ^= 1u64 << sq;
                assert_ne!(slot, base, "{piece:?} on {sq}");
            }
        }
    }

    #[test]
    fn gravity_keeps_entries_within_bounds() {
        let largest_bonus = crate::search_params::ParamSpec::find("HistoryBonusMax")
            .unwrap()
            .max;
        let mut entry = 0i16;
        for _ in 0..1000 {
            apply_gravity(&mut entry, largest_bonus);
        }
        assert!(i32::from(entry) <= HISTORY_MAX);
        for _ in 0..1000 {
            apply_gravity(&mut entry, -largest_bonus);
        }
        assert!(i32::from(entry) >= -HISTORY_MAX);
    }

    #[test]
    fn bonus_grows_with_depth_and_is_capped() {
        let params = SearchParams::default();
        assert!(stat_bonus(2, &params) > stat_bonus(1, &params));
        assert_eq!(stat_bonus(50, &params), params.history_bonus_max);
    }

    #[test]
    fn quiet_update_touches_butterfly_and_continuation_tables() {
        let mut histories = Histories::new();
        let m = Move::quiet(Square::new(12), Square::new(28));
        let key = ContinuationKey::new(Piece::BN, Square::new(45));

        histories.update_quiet(Color::White, m, Piece::WP, &[Some(key), None], 400);

        assert_eq!(histories.butterfly(Color::White, m), 400);
        assert_eq!(histories.continuation(key, Piece::WP, Square::new(28)), 400);
        assert_eq!(histories.butterfly(Color::Black, m), 0);
    }
}
