use crate::board::Board;
use crate::history::{ContinuationKey, Histories, captured_kind};
use crate::see::{PIECE_VALUES, see, see_ge};
use crate::types::{MAX_MOVES, Move, MoveList, PieceKind, StackVec};

const MAX_BAD_CAPTURES: usize = 64;
const MVV_SCALE: i32 = 8;

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
    fn quiet_score(&self, board: &Board, m: Move) -> i32 {
        let piece = board.piece_on[m.from as usize];
        let continuation: i32 = self
            .continuations
            .iter()
            .flatten()
            .map(|&key| self.histories.continuation(key, piece, m.to))
            .sum();
        self.histories.butterfly(board.turn, m) + continuation
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
                    self.scores.clear();
                    for &m in self.moves.as_slice() {
                        self.scores.push(ctx.quiet_score(board, m));
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
