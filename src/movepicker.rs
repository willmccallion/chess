use crate::board::Board;
use crate::see::see;
use crate::types::{Move, MoveList};

const GOOD_CAPTURE_BASE: i32 = 1_000_000;

// ── MovePicker for negamax ──────────────────────────────────────────

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

pub struct MovePicker {
    stage: Stage,
    tt_move: Option<Move>,
    killer1: Option<Move>,
    killer2: Option<Move>,
    counter_move: Option<Move>,

    moves: MoveList,
    scores: [i32; 300],
    idx: usize,

    bad_captures: [Move; 64],
    bad_count: usize,
    bad_idx: usize,
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
            scores: [0; 300],
            idx: 0,
            bad_captures: [Move::default(); 64],
            bad_count: 0,
            bad_idx: 0,
        }
    }

    pub fn next(&mut self, board: &Board, history: &[[i32; 64]; 13]) -> Option<Move> {
        loop {
            match self.stage {
                Stage::TtMove => {
                    self.stage = Stage::GenCaptures;
                    if let Some(m) = self.tt_move {
                        if board.is_pseudo_legal(m) {
                            return Some(m);
                        }
                    }
                }

                Stage::GenCaptures => {
                    board.generate_captures(&mut self.moves);
                    for i in 0..self.moves.len() {
                        let m = self.moves.moves[i];
                        let see_val = see(board, m);
                        self.scores[i] = if see_val >= 0 {
                            GOOD_CAPTURE_BASE + see_val
                        } else {
                            see_val // negative => bad capture
                        };
                    }
                    self.idx = 0;
                    self.stage = Stage::GoodCaptures;
                }

                Stage::GoodCaptures => {
                    while self.idx < self.moves.len() {
                        // Selection sort: find best remaining
                        let mut best_k = self.idx;
                        let mut best_s = self.scores[self.idx];
                        for j in (self.idx + 1)..self.moves.len() {
                            if self.scores[j] > best_s {
                                best_s = self.scores[j];
                                best_k = j;
                            }
                        }
                        self.moves.swap(self.idx, best_k);
                        self.scores.swap(self.idx, best_k);

                        let m = self.moves.moves[self.idx];
                        let s = self.scores[self.idx];
                        self.idx += 1;

                        // Skip if already yielded as TT move
                        if Some(m) == self.tt_move {
                            continue;
                        }

                        if s < GOOD_CAPTURE_BASE {
                            // Bad capture: stash for later
                            if self.bad_count < 64 {
                                self.bad_captures[self.bad_count] = m;
                                self.bad_count += 1;
                            }
                            continue;
                        }

                        return Some(m);
                    }
                    self.stage = Stage::Killer1;
                }

                Stage::Killer1 => {
                    self.stage = Stage::Killer2;
                    if let Some(m) = self.killer1 {
                        if Some(m) != self.tt_move
                            && !m.capture
                            && m.promotion.is_none()
                            && board.is_pseudo_legal(m)
                        {
                            return Some(m);
                        }
                    }
                }

                Stage::Killer2 => {
                    self.stage = Stage::CounterMove;
                    if let Some(m) = self.killer2 {
                        if Some(m) != self.tt_move
                            && Some(m) != self.killer1
                            && !m.capture
                            && m.promotion.is_none()
                            && board.is_pseudo_legal(m)
                        {
                            return Some(m);
                        }
                    }
                }

                Stage::CounterMove => {
                    self.stage = Stage::GenQuiets;
                    if let Some(m) = self.counter_move {
                        if Some(m) != self.tt_move
                            && Some(m) != self.killer1
                            && Some(m) != self.killer2
                            && !m.capture
                            && m.promotion.is_none()
                            && board.is_pseudo_legal(m)
                        {
                            return Some(m);
                        }
                    }
                }

                Stage::GenQuiets => {
                    board.generate_quiets(&mut self.moves);
                    for i in 0..self.moves.len() {
                        let m = self.moves.moves[i];
                        let piece_idx = board.piece_on[m.from as usize].index();
                        self.scores[i] = history[piece_idx][m.to as usize];
                    }
                    self.idx = 0;
                    self.stage = Stage::Quiets;
                }

                Stage::Quiets => {
                    while self.idx < self.moves.len() {
                        let mut best_k = self.idx;
                        let mut best_s = self.scores[self.idx];
                        for j in (self.idx + 1)..self.moves.len() {
                            if self.scores[j] > best_s {
                                best_s = self.scores[j];
                                best_k = j;
                            }
                        }
                        self.moves.swap(self.idx, best_k);
                        self.scores.swap(self.idx, best_k);

                        let m = self.moves.moves[self.idx];
                        self.idx += 1;

                        // Skip duplicates
                        if Some(m) == self.tt_move
                            || Some(m) == self.killer1
                            || Some(m) == self.killer2
                            || Some(m) == self.counter_move
                        {
                            continue;
                        }

                        return Some(m);
                    }
                    self.stage = Stage::BadCaptures;
                    self.bad_idx = 0;
                }

                Stage::BadCaptures => {
                    if self.bad_idx < self.bad_count {
                        let m = self.bad_captures[self.bad_idx];
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

// ── QMovePicker for quiescence search ───────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum QStage {
    GenCaptures,
    Captures,
    Done,
}

pub struct QMovePicker {
    stage: QStage,
    moves: MoveList,
    scores: [i32; 300],
    idx: usize,
}

impl QMovePicker {
    pub fn new() -> Self {
        Self {
            stage: QStage::GenCaptures,
            moves: MoveList::new(),
            scores: [0; 300],
            idx: 0,
        }
    }

    pub fn next(&mut self, board: &Board) -> Option<Move> {
        loop {
            match self.stage {
                QStage::GenCaptures => {
                    board.generate_captures(&mut self.moves);
                    for i in 0..self.moves.len() {
                        let m = self.moves.moves[i];
                        self.scores[i] = see(board, m);
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
                    let mut best_s = self.scores[self.idx];
                    for j in (self.idx + 1)..self.moves.len() {
                        if self.scores[j] > best_s {
                            best_s = self.scores[j];
                            best_k = j;
                        }
                    }
                    self.moves.swap(self.idx, best_k);
                    self.scores.swap(self.idx, best_k);

                    let m = self.moves.moves[self.idx];
                    self.idx += 1;
                    return Some(m);
                }

                QStage::Done => return None,
            }
        }
    }
}
