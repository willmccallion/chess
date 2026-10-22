use crate::board::Board;
use crate::history::{ContinuationKey, Histories, captured_kind};
use crate::magics;
use crate::see::{PIECE_VALUES, see, see_ge};
use crate::types::{Bitboard, Color, MAX_MOVES, Move, MoveList, Piece, PieceKind, StackVec};

const MAX_BAD_CAPTURES: usize = 64;
const MVV_SCALE: i32 = 8;
const QUEEN_THREAT_BONUS: i32 = 20_000;
const ROOK_THREAT_BONUS: i32 = 10_000;
const MINOR_THREAT_BONUS: i32 = 6_000;
const CHECK_BONUS: i32 = 6_000;

/// Squares the opponent attacks with cheap pieces, and squares that check their king.
struct QuietThreats {
    by_pawn: Bitboard,
    /// Attacked by a pawn or minor piece.
    by_minor: Bitboard,
    /// Attacked by a pawn, minor piece or rook.
    by_rook: Bitboard,
    checks: [Bitboard; 6],
}

fn attacks_of(board: &Board, kind: PieceKind, color: Color) -> Bitboard {
    let mut pieces = board.piece_bb[Piece::from_kind(kind, color).index()];
    let mut attacks = 0;
    while pieces != 0 {
        let sq = pieces.trailing_zeros() as usize;
        pieces &= pieces - 1;
        attacks |= match kind {
            PieceKind::Pawn if color == Color::White => magics::WHITE_PAWN_ATTACKS[sq],
            PieceKind::Pawn => magics::BLACK_PAWN_ATTACKS[sq],
            PieceKind::Knight => magics::knight_attacks_from(sq),
            PieceKind::Bishop => magics::get_bishop_attacks(sq, board.all_pieces),
            PieceKind::Rook => magics::get_rook_attacks(sq, board.all_pieces),
            PieceKind::Queen | PieceKind::King => 0,
        };
    }
    attacks
}

impl QuietThreats {
    fn new(board: &Board) -> Self {
        let them = board.turn.other();
        let by_pawn = attacks_of(board, PieceKind::Pawn, them);
        let by_minor = by_pawn
            | attacks_of(board, PieceKind::Knight, them)
            | attacks_of(board, PieceKind::Bishop, them);
        let by_rook = by_minor | attacks_of(board, PieceKind::Rook, them);

        let king_sq = board.king_square(them) as usize;
        let pawn_checks = match board.turn {
            Color::White => magics::BLACK_PAWN_ATTACKS[king_sq],
            Color::Black => magics::WHITE_PAWN_ATTACKS[king_sq],
        };
        let bishop_checks = magics::get_bishop_attacks(king_sq, board.all_pieces);
        let rook_checks = magics::get_rook_attacks(king_sq, board.all_pieces);
        let mut checks = [0; 6];
        checks[PieceKind::Pawn as usize] = pawn_checks;
        checks[PieceKind::Knight as usize] = magics::knight_attacks_from(king_sq);
        checks[PieceKind::Bishop as usize] = bishop_checks;
        checks[PieceKind::Rook as usize] = rook_checks;
        checks[PieceKind::Queen as usize] = bishop_checks | rook_checks;
        Self {
            by_pawn,
            by_minor,
            by_rook,
            checks,
        }
    }

    /// Rewards moving a piece out of attack by a cheaper one and giving check;
    /// penalizes moving into such an attack.
    fn adjustment(&self, kind: PieceKind, m: Move) -> i32 {
        let (from, to) = (1u64 << m.from, 1u64 << m.to);
        let (danger, bonus) = match kind {
            PieceKind::Queen => (self.by_rook, QUEEN_THREAT_BONUS),
            PieceKind::Rook => (self.by_minor, ROOK_THREAT_BONUS),
            PieceKind::Knight | PieceKind::Bishop => (self.by_pawn, MINOR_THREAT_BONUS),
            PieceKind::Pawn | PieceKind::King => (0, 0),
        };
        let escape = if danger & from != 0 && danger & to == 0 {
            bonus
        } else {
            0
        };
        let blunder = if danger & to != 0 { bonus } else { 0 };
        let check = if self.checks[kind as usize] & to != 0 {
            CHECK_BONUS
        } else {
            0
        };
        escape - blunder + check
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    TtMove,
    GenCaptures,
    GoodCaptures,
    Killer1,
    Killer2,
    CounterMove,
    GenQuiets,
    Quiets,
    BadCaptures,
    Done,
}

impl Stage {
    fn is_quiet_stage(self) -> bool {
        matches!(
            self,
            Stage::Killer1 | Stage::Killer2 | Stage::CounterMove | Stage::GenQuiets | Stage::Quiets
        )
    }
}

/// History tables and the continuation keys of the previous plies, for scoring moves.
pub struct OrderingContext<'a> {
    pub histories: &'a Histories,
    pub continuations: [Option<ContinuationKey>; 2],
}

impl OrderingContext<'_> {
    fn quiet_score(&self, board: &Board, threats: &QuietThreats, m: Move) -> i32 {
        let piece = board.piece_on[m.from as usize];
        let threat = piece.kind().map_or(0, |kind| threats.adjustment(kind, m));
        let continuation: i32 = self
            .continuations
            .iter()
            .flatten()
            .map(|&key| self.histories.continuation(key, piece, m.to))
            .sum();
        self.histories.butterfly(board.turn, m) + continuation + threat
    }

    fn capture_score(&self, board: &Board, m: Move) -> i32 {
        let promotion_value = match m.promotion {
            Some(PieceKind::Queen) => PIECE_VALUES[PieceKind::Queen as usize],
            _ => 0,
        };
        let piece = board.piece_on[m.from as usize];
        let victim_score = captured_kind(board.piece_on[m.to as usize], m).map_or(0, |victim| {
            MVV_SCALE * PIECE_VALUES[victim as usize] + self.histories.capture(piece, m.to, victim)
        });
        victim_score + MVV_SCALE * promotion_value
    }
}

/// Index of the highest-scoring entry in `scores[start..]`.
fn best_index_from(scores: &[i32], start: usize) -> usize {
    let mut best = start;
    for i in start + 1..scores.len() {
        if scores[i] > scores[best] {
            best = i;
        }
    }
    best
}

pub struct MovePicker {
    stage: Stage,
    tt_move: Option<Move>,
    killer1: Option<Move>,
    killer2: Option<Move>,
    counter_move: Option<Move>,

    moves: MoveList,
    scores: StackVec<i32, MAX_MOVES>,
    idx: usize,

    bad_captures: StackVec<Move, MAX_BAD_CAPTURES>,
    bad_idx: usize,
    skip_quiets: bool,
}

impl MovePicker {
    pub fn new(
        tt_move: Option<Move>,
        killer1: Option<Move>,
        killer2: Option<Move>,
        counter_move: Option<Move>,
    ) -> Self {
        Self {
            stage: if tt_move.is_some() {
                Stage::TtMove
            } else {
                Stage::GenCaptures
            },
            tt_move,
            killer1,
            killer2,
            counter_move,
            moves: MoveList::new(),
            scores: StackVec::default(),
            idx: 0,
            bad_captures: StackVec::default(),
            bad_idx: 0,
            skip_quiets: false,
        }
    }

    /// Stops yielding quiet moves (killers and counter-move included); captures continue.
    pub fn skip_quiets(&mut self) {
        self.skip_quiets = true;
    }

    fn is_fresh_quiet_special(&self, m: Move, board: &Board, earlier: &[Option<Move>]) -> bool {
        Some(m) != self.tt_move
            && !earlier.contains(&Some(m))
            && !m.capture
            && m.promotion.is_none()
            && board.is_pseudo_legal(m)
    }

    /// Selection-sorts the next move into place and returns it with its score.
    fn pick_next(&mut self) -> Option<(Move, i32)> {
        if self.idx >= self.moves.len() {
            return None;
        }
        let best = best_index_from(self.scores.as_slice(), self.idx);
        self.moves.swap(self.idx, best);
        self.scores.as_mut_slice().swap(self.idx, best);
        let picked = (self.moves.get(self.idx), self.scores.as_slice()[self.idx]);
        self.idx += 1;
        Some(picked)
    }

    pub fn next(&mut self, board: &Board, ctx: &OrderingContext) -> Option<Move> {
        loop {
            if self.skip_quiets && self.stage.is_quiet_stage() {
                self.stage = Stage::BadCaptures;
            }
            match self.stage {
                Stage::TtMove => {
                    self.stage = Stage::GenCaptures;
                    if let Some(m) = self.tt_move
                        && board.is_pseudo_legal(m)
                    {
                        return Some(m);
                    }
                }

                Stage::GenCaptures => {
                    board.generate_captures(&mut self.moves);
                    self.scores.clear();
                    for &m in self.moves.as_slice() {
                        self.scores.push(ctx.capture_score(board, m));
                    }
                    self.idx = 0;
                    self.stage = Stage::GoodCaptures;
                }

                Stage::GoodCaptures => {
                    while let Some((m, _)) = self.pick_next() {
                        if Some(m) == self.tt_move {
                            continue;
                        }
                        if !see_ge(board, m, 0) {
                            if self.bad_captures.len() < MAX_BAD_CAPTURES {
                                self.bad_captures.push(m);
                            }
                            continue;
                        }
                        return Some(m);
                    }
                    self.stage = Stage::Killer1;
                }

                Stage::Killer1 => {
                    self.stage = Stage::Killer2;
                    if let Some(m) = self.killer1
                        && self.is_fresh_quiet_special(m, board, &[])
                    {
                        return Some(m);
                    }
                }

                Stage::Killer2 => {
                    self.stage = Stage::CounterMove;
                    if let Some(m) = self.killer2
                        && self.is_fresh_quiet_special(m, board, &[self.killer1])
                    {
                        return Some(m);
                    }
                }

                Stage::CounterMove => {
                    self.stage = Stage::GenQuiets;
                    if let Some(m) = self.counter_move
                        && self.is_fresh_quiet_special(m, board, &[self.killer1, self.killer2])
                    {
                        return Some(m);
                    }
                }

                Stage::GenQuiets => {
                    board.generate_quiets(&mut self.moves);
                    let threats = QuietThreats::new(board);
                    self.scores.clear();
                    for &m in self.moves.as_slice() {
                        self.scores.push(ctx.quiet_score(board, &threats, m));
                    }
                    self.idx = 0;
                    self.stage = Stage::Quiets;
                }

                Stage::Quiets => {
                    while let Some((m, _)) = self.pick_next() {
                        let already_tried = Some(m) == self.tt_move
                            || Some(m) == self.killer1
                            || Some(m) == self.killer2
                            || Some(m) == self.counter_move;
                        if !already_tried {
                            return Some(m);
                        }
                    }
                    self.stage = Stage::BadCaptures;
                }

                Stage::BadCaptures => {
                    if self.bad_idx < self.bad_captures.len() {
                        let m = self.bad_captures.as_slice()[self.bad_idx];
                        self.bad_idx += 1;
                        return Some(m);
                    }
                    self.stage = Stage::Done;
                }

                Stage::Done => return None,
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QStage {
    GenCaptures,
    Captures,
    Done,
}

pub struct QMovePicker {
    stage: QStage,
    moves: MoveList,
    scores: StackVec<i32, MAX_MOVES>,
    idx: usize,
}

impl QMovePicker {
    pub fn new() -> Self {
        Self {
            stage: QStage::GenCaptures,
            moves: MoveList::new(),
            scores: StackVec::default(),
            idx: 0,
        }
    }

    pub fn next(&mut self, board: &Board) -> Option<Move> {
        loop {
            match self.stage {
                QStage::GenCaptures => {
                    board.generate_captures(&mut self.moves);
                    self.scores.clear();
                    for &m in self.moves.as_slice() {
                        self.scores.push(see(board, m));
                    }
                    self.idx = 0;
                    self.stage = QStage::Captures;
                }

                QStage::Captures => {
                    if self.idx >= self.moves.len() {
                        self.stage = QStage::Done;
                        return None;
                    }

                    // Selection sort
                    let mut best_k = self.idx;
                    let mut best_s = self.scores.as_slice()[self.idx];
                    for j in (self.idx + 1)..self.moves.len() {
                        if self.scores.as_slice()[j] > best_s {
                            best_s = self.scores.as_slice()[j];
                            best_k = j;
                        }
                    }
                    self.moves.swap(self.idx, best_k);
                    self.scores.as_mut_slice().swap(self.idx, best_k);

                    let m = self.moves.get(self.idx);
                    self.idx += 1;
                    return Some(m);
                }

                QStage::Done => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_ordering_rescues_a_queen_attacked_by_a_pawn_first() {
        let board = Board::from_fen("4k3/8/8/2p5/3Q4/8/P7/4K3 w - - 0 1").unwrap();
        let histories = Histories::new();
        let ctx = OrderingContext {
            histories: &histories,
            continuations: [None, None],
        };
        let mut picker = MovePicker::new(None, None, None, None);

        let first_quiet = std::iter::from_fn(|| picker.next(&board, &ctx))
            .find(|m| !m.capture)
            .unwrap();

        assert_eq!(first_quiet.from, 27, "{first_quiet:?}");
        assert_eq!(
            QuietThreats::new(&board).by_pawn & (1u64 << first_quiet.to),
            0
        );
    }

    #[test]
    fn moving_a_rook_into_a_knight_attack_is_penalized() {
        let board = Board::from_fen("4k3/8/8/3n4/8/R7/8/4K3 w - - 0 1").unwrap();
        let threats = QuietThreats::new(&board);

        let into_attack = threats.adjustment(PieceKind::Rook, Move::quiet(16, 18));
        let safe = threats.adjustment(PieceKind::Rook, Move::quiet(16, 17));

        assert!(into_attack < safe, "{into_attack} {safe}");
    }
}
