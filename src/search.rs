use crate::board::Board;
use crate::nnue::evaluate;
use crate::see::see;
use crate::tt::{Bound, SharedTransTable};
use crate::types::{Move, MoveList, Piece, PieceKind};
use crate::uci_io::format_uci;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub const MATE_SCORE: i32 = 30_000;
const MATE_THRESHOLD: i32 = MATE_SCORE - 512;
const MAX_PLY: usize = 128;
const DRAW_SCORE: i32 = 0;

const FUTILITY_MARGIN: [i32; 8] = [0, 125, 250, 450, 700, 950, 1200, 1500];
const LMP_LIMITS: [i32; 4] = [0, 3, 5, 8];
const HISTORY_PRUNE_THRESHOLD: i32 = 4000;
const IID_MIN_DEPTH: i32 = 5;
const RAZORING_MARGIN: i32 = 400;
const DELTA_MARGIN: i32 = 200;

const HISTORY_MAX: i32 = 16_384;
const PIECE_VALUES: [i32; 6] = [100, 320, 330, 500, 900, 20000];

struct TimeManager {
    start_time: Instant,
    soft_limit: u64,
    hard_limit: u64,
    stop_signal: Arc<AtomicBool>,
    is_main_thread: bool,
    nodes: u64,
    best_move_changes: usize,
    best_move_stability: usize,
    previous_best_move: Option<Move>,
}

impl TimeManager {
    fn new(time_ms: u64, stop_signal: Arc<AtomicBool>, is_main_thread: bool) -> Self {
        let (soft, hard) = (time_ms, time_ms);

        Self {
            start_time: Instant::now(),
            soft_limit: soft,
            hard_limit: hard,
            stop_signal,
            is_main_thread,
            nodes: 0,
            best_move_changes: 0,
            best_move_stability: 0,
            previous_best_move: None,
        }
    }

    #[inline(always)]
    fn check_hard_limit(&mut self) -> bool {
        if self.stop_signal.load(Ordering::Relaxed) {
            return true;
        }
        if self.is_main_thread && (self.nodes & 4095) == 0 {
            let elapsed = self.start_time.elapsed().as_millis() as u64;
            if elapsed >= self.hard_limit {
                self.stop_signal.store(true, Ordering::Relaxed);
                return true;
            }
        }
        false
    }

    fn check_soft_limit(&mut self, depth: usize, best_move: Option<Move>) -> bool {
        if self.stop_signal.load(Ordering::Relaxed) {
            return true;
        }
        if !self.is_main_thread {
            return false;
        }

        let elapsed = self.start_time.elapsed().as_millis() as u64;

        if best_move != self.previous_best_move {
            self.best_move_changes += 1;
            self.best_move_stability = 0;
        } else {
            self.best_move_stability += 1;
        }
        self.previous_best_move = best_move;

        if elapsed >= self.hard_limit {
            self.stop_signal.store(true, Ordering::Relaxed);
            return true;
        }

        if elapsed >= self.soft_limit {
            let extension_limit = (self.soft_limit as f64 * 1.5) as u64;

            if self.best_move_stability == 0 && elapsed < extension_limit.min(self.hard_limit) {
                return false;
            }

            self.stop_signal.store(true, Ordering::Relaxed);
            return true;
        }

        if depth > 8 && self.best_move_stability >= 4 && elapsed > (self.soft_limit / 2) {
            self.stop_signal.store(true, Ordering::Relaxed);
            return true;
        }

        false
    }
}

pub struct Search<'a> {
    board: Board,
    tt: &'a SharedTransTable,
    tm: TimeManager,
    killers: [[Option<Move>; 2]; MAX_PLY],
    history: [[i32; 64]; 13],
    counter_moves: [[[Option<Move>; 64]; 13]; 2],
    ply: usize,
    seldepth: usize,
    prev_move: [Option<Move>; MAX_PLY],
}

fn quiesce(s: &mut Search, mut alpha: i32, beta: i32) -> i32 {
    use crate::movepicker::QMovePicker;

    s.seldepth = s.seldepth.max(s.ply);
    s.tm.nodes += 1;

    if s.tm.check_hard_limit() {
        return 0;
    }

    let king_sq = s.board.piece_bb[Piece::from_kind(PieceKind::King, s.board.turn).index()]
        .trailing_zeros() as i32;
    let in_check = s.board.is_square_attacked(king_sq, s.board.turn.other());

    let stand_pat = if !in_check {
        let val = evaluate(&s.board);
        if val >= beta {
            return beta;
        }
        if val > alpha {
            alpha = val;
        }
        val
    } else {
        -MATE_SCORE
    };

    if in_check {
        // In check: need all moves (including quiet evasions) for checkmate detection.
        // Use the old approach — this path is rare.
        let mut move_list = MoveList::new();
        s.board.generate_pseudo_legal_moves(&mut move_list);

        let mut scores = [0i32; 300];
        for i in 0..move_list.len() {
            let m = move_list.moves[i];
            if m.capture {
                scores[i] = see(&s.board, m);
            } else {
                scores[i] = 0;
            }
        }

        let mut legal_moves_found = false;

        for i in 0..move_list.len() {
            let mut best_idx = i;
            let mut best_score = scores[i];
            for j in (i + 1)..move_list.len() {
                if scores[j] > best_score {
                    best_score = scores[j];
                    best_idx = j;
                }
            }
            move_list.swap(i, best_idx);
            scores.swap(i, best_idx);

            let m = move_list.moves[i];

            let undo = s.board.make_move(m);
            let us = s.board.turn.other();
            let king_bb = s.board.piece_bb[Piece::from_kind(PieceKind::King, us).index()];
            if king_bb != 0
                && s.board
                    .is_square_attacked(king_bb.trailing_zeros() as i32, s.board.turn)
            {
                s.board.unmake_move(m, undo);
                continue;
            }
            legal_moves_found = true;

            s.ply += 1;
            s.prev_move[s.ply] = Some(m);
            let score = -quiesce(s, -beta, -alpha);
            s.ply -= 1;
            s.board.unmake_move(m, undo);

            if score >= beta {
                return beta;
            }
            if score > alpha {
                alpha = score;
            }
        }

        if !legal_moves_found {
            return -MATE_SCORE + s.ply as i32;
        }
        return alpha;
    }

    // Not in check: only search captures + promotions via QMovePicker.
    let mut picker = QMovePicker::new();
    while let Some(m) = picker.next(&s.board) {
        // Delta pruning
        if m.capture && m.promotion.is_none() {
            let captured = s.board.piece_on[m.to as usize];
            if !captured.is_empty() {
                let piece_val = PIECE_VALUES[captured.kind().unwrap() as usize];
                if stand_pat + piece_val + DELTA_MARGIN < alpha {
                    continue;
                }
            }
        }
        // SEE pruning
        if m.capture && see(&s.board, m) < 0 {
            continue;
        }

        let undo = s.board.make_move(m);
        let us = s.board.turn.other();
        let king_bb = s.board.piece_bb[Piece::from_kind(PieceKind::King, us).index()];
        if king_bb != 0
            && s.board
                .is_square_attacked(king_bb.trailing_zeros() as i32, s.board.turn)
        {
            s.board.unmake_move(m, undo);
            continue;
        }

        s.ply += 1;
        s.prev_move[s.ply] = Some(m);
        let score = -quiesce(s, -beta, -alpha);
        s.ply -= 1;
        s.board.unmake_move(m, undo);

        if score >= beta {
            return beta;
        }
        if score > alpha {
            alpha = score;
        }
    }

    alpha
}

fn negamax(s: &mut Search, mut alpha: i32, beta: i32, mut depth: i32) -> i32 {
    s.seldepth = s.seldepth.max(s.ply);
    if s.tm.check_hard_limit() {
        return 0;
    }

    if s.ply > 0 && (s.board.is_draw_by_repetition() || s.board.halfmove_clock >= 100) {
        return DRAW_SCORE;
    }
    if s.ply >= MAX_PLY - 1 {
        return evaluate(&s.board);
    }

    let is_pv = beta - alpha > 1;
    let alpha_orig = alpha;
    let key = s.board.zobrist;
    let mut tt_move: Option<Move> = None;

    if let Some(entry) = s.tt.probe(key) {
        if entry.depth() >= depth as i16 && s.ply > 0 {
            let mut score = entry.score();
            if score.abs() > MATE_THRESHOLD {
                if score > 0 {
                    score -= s.ply as i32;
                } else {
                    score += s.ply as i32;
                }
            }
            match entry.bound() {
                Bound::Exact => return score,
                Bound::Lower if score >= beta => return score,
                Bound::Upper if score <= alpha => return score,
                _ => {}
            }
        }
        tt_move = entry.best_move();
    }

    let king_sq = s.board.piece_bb[Piece::from_kind(PieceKind::King, s.board.turn).index()]
        .trailing_zeros() as i32;
    let in_check = s.board.is_square_attacked(king_sq, s.board.turn.other());

    if in_check {
        depth += 1;
    }
    if depth <= 0 {
        return quiesce(s, alpha, beta);
    }

    s.tm.nodes += 1;

    let mut static_eval = MATE_SCORE;
    if !is_pv && !in_check {
        static_eval = evaluate(&s.board);
    }

    if !is_pv && !in_check && depth <= 3 && static_eval + RAZORING_MARGIN + 100 * depth < alpha {
        let q_score = quiesce(s, alpha, beta);
        if q_score < alpha {
            return alpha;
        }
    }
    if !is_pv && !in_check && depth < 8 && static_eval - FUTILITY_MARGIN[depth as usize] >= beta {
        return beta;
    }
    if is_pv && depth >= IID_MIN_DEPTH && tt_move.is_none() && !s.tm.check_hard_limit() {
        let _ = negamax(s, alpha, beta, depth - 2);
        if let Some(entry) = s.tt.probe(key) {
            tt_move = entry.best_move();
        }
    }

    let our_pieces = if s.board.turn == crate::types::Color::White {
        s.board.w_pieces
    } else {
        s.board.b_pieces
    };
    let non_pawn_king_material = our_pieces
        & !(s.board.piece_bb[Piece::WP.index()]
            | s.board.piece_bb[Piece::BP.index()]
            | s.board.piece_bb[Piece::WK.index()]
            | s.board.piece_bb[Piece::BK.index()]);

    if !is_pv && !in_check && depth >= 3 && non_pawn_king_material != 0 {
        let r = 3 + depth / 6;
        let undo = s.board.make_null_move();
        s.ply += 1;
        let null_score = -negamax(s, -beta, -beta + 1, depth - r);
        s.ply -= 1;
        s.board.unmake_null_move(undo);
        if null_score >= beta {
            if depth < 10 {
                return beta;
            }
            let verification_score = negamax(s, beta - 1, beta, depth - 6);
            if verification_score >= beta {
                return beta;
            }
        }
    }

    // Determine killer and counter move for this ply
    let killer1 = s.killers[s.ply][0];
    let killer2 = s.killers[s.ply][1];
    let counter_move = s.prev_move[s.ply.saturating_sub(1)].and_then(|prev_m| {
        let piece_idx = s.board.piece_on[prev_m.from as usize].index();
        s.counter_moves[prev_m.capture as usize][piece_idx][prev_m.to as usize]
    });

    let mut picker =
        crate::movepicker::MovePicker::new(tt_move, killer1, killer2, counter_move);
    let mut best_score = -MATE_SCORE;
    let mut best_move: Option<Move> = None;
    let mut moves_searched = 0;
    let mut searched_quiets = [Move::default(); 128];
    let mut num_searched_quiets: usize = 0;

    while let Some(m) = picker.next(&s.board, &s.history) {
        // LMP: skip quiet moves after threshold
        if !is_pv && !in_check && depth <= 3 && !m.capture && m.promotion.is_none() {
            let lmp_limit = LMP_LIMITS[depth as usize];
            if moves_searched as i32 >= lmp_limit {
                continue;
            }
        }
        // History pruning
        if depth <= 2 && !in_check && !m.capture && m.promotion.is_none() {
            let piece_idx = s.board.piece_on[m.from as usize].index();
            let hist_score = s.history[piece_idx][m.to as usize];
            if hist_score < -HISTORY_PRUNE_THRESHOLD {
                continue;
            }
        }

        // Make move + legality check
        let undo = s.board.make_move(m);
        let us = s.board.turn.other();
        let king_bb = s.board.piece_bb[Piece::from_kind(PieceKind::King, us).index()];
        if king_bb != 0
            && s.board
                .is_square_attacked(king_bb.trailing_zeros() as i32, s.board.turn)
        {
            s.board.unmake_move(m, undo);
            continue;
        }

        s.ply += 1;
        s.prev_move[s.ply] = Some(m);
        moves_searched += 1;

        let score;
        if moves_searched == 1 {
            score = -negamax(s, -beta, -alpha, depth - 1);
        } else {
            // Bad capture pruning
            if depth < 8 && !in_check && m.capture && see(&s.board, m) < 0 {
                s.ply -= 1;
                s.board.unmake_move(m, undo);
                continue;
            }

            // LMR
            let mut reduction = 0;
            if depth >= 3 && !m.capture && !in_check {
                let d = depth as f32;
                let mn = moves_searched as f32;
                reduction = (0.5 + d.ln() * mn.ln() / 2.0) as i32;
                if !is_pv {
                    reduction += 1;
                }
                let history_score =
                    s.history[s.board.piece_on[m.from as usize].index()][m.to as usize];
                reduction -= history_score / 4096;
                reduction = reduction.clamp(0, depth - 2);
            }

            let mut search_score = -negamax(s, -alpha - 1, -alpha, depth - 1 - reduction);
            if search_score > alpha && reduction > 0 {
                search_score = -negamax(s, -alpha - 1, -alpha, depth - 1);
            }
            if search_score > alpha && search_score < beta {
                search_score = -negamax(s, -beta, -alpha, depth - 1);
            }
            score = search_score;
        };

        s.ply -= 1;
        s.board.unmake_move(m, undo);

        if s.tm.check_hard_limit() {
            return 0;
        }

        if score > best_score {
            best_score = score;
            best_move = Some(m);
            if score > alpha {
                alpha = score;
                if alpha >= beta {
                    // Beta cutoff — update killer, counter, history
                    if !m.capture {
                        if Some(m) != s.killers[s.ply][0] {
                            s.killers[s.ply][1] = s.killers[s.ply][0];
                            s.killers[s.ply][0] = Some(m);
                        }

                        if let Some(prev_m) = s.prev_move[s.ply.saturating_sub(1)] {
                            let piece_idx = s.board.piece_on[prev_m.from as usize].index();
                            s.counter_moves[prev_m.capture as usize][piece_idx]
                                [prev_m.to as usize] = Some(m);
                        }

                        let piece_idx = s.board.piece_on[m.from as usize].index();
                        let bonus = (depth * depth).min(1000);
                        s.history[piece_idx][m.to as usize] += bonus;

                        if s.history[piece_idx][m.to as usize] > HISTORY_MAX {
                            for p in 1..13 {
                                for sq in 0..64 {
                                    s.history[p][sq] >>= 1;
                                }
                            }
                        }

                        // History malus for previously searched quiet moves
                        for k in 0..num_searched_quiets {
                            let failed_move = searched_quiets[k];
                            let p_idx = s.board.piece_on[failed_move.from as usize].index();
                            s.history[p_idx][failed_move.to as usize] -= bonus;
                        }
                    }
                    break;
                }
            }
        }

        // Track searched quiets for history malus (only non-cutoff moves reach here)
        if !m.capture && m.promotion.is_none() && num_searched_quiets < 128 {
            searched_quiets[num_searched_quiets] = m;
            num_searched_quiets += 1;
        }
    }

    if moves_searched == 0 {
        return if in_check {
            -MATE_SCORE + s.ply as i32
        } else {
            DRAW_SCORE
        };
    }

    let bound = if best_score <= alpha_orig {
        Bound::Upper
    } else if best_score >= beta {
        Bound::Lower
    } else {
        Bound::Exact
    };
    let mut score_to_store = best_score;
    if score_to_store.abs() > MATE_THRESHOLD {
        if score_to_store > 0 {
            score_to_store += s.ply as i32;
        } else {
            score_to_store -= s.ply as i32;
        }
    }
    s.tt.store(key, depth as i16, score_to_store, bound, best_move);
    best_score
}

pub fn get_pv_from_tt(mut pos: Board, tt: &SharedTransTable, max_len: usize) -> Vec<Move> {
    let mut pv = Vec::with_capacity(max_len);
    for _ in 0..max_len {
        if let Some(m) = tt.probe(pos.zobrist).and_then(|e| e.best_move()) {
            pv.push(m);
            pos.make_move(m);
        } else {
            break;
        }
    }
    pv
}

pub fn best_move_timed(
    b: &Board,
    tt: &mut SharedTransTable,
    time_ms: u64,
    max_depth: usize,
    stop_signal: Arc<AtomicBool>,
    is_main_thread: bool,
) -> (Option<Move>, usize, u64) {
    if is_main_thread {
        tt.tick_age();
    }

    if is_main_thread && time_ms < u64::MAX / 2 {
        let mut legal_moves = MoveList::new();
        let mut temp_board = b.clone();
        temp_board.generate_legal_moves(&mut legal_moves);
        if legal_moves.len() == 1 {
            return (Some(legal_moves.moves[0]), 1, 0);
        }
    }

    let soft_limit = time_ms;
    let hard_limit = if time_ms > 300_000 {
        time_ms
    } else {
        time_ms * 4
    };

    let mut search = Search {
        board: b.clone(),
        tt,
        tm: TimeManager::new(soft_limit, stop_signal, is_main_thread),
        killers: [[None; 2]; MAX_PLY],
        history: [[0; 64]; 13],
        counter_moves: [[[None; 64]; 13]; 2],
        ply: 0,
        seldepth: 0,
        prev_move: [None; MAX_PLY],
    };

    search.tm.soft_limit = soft_limit;
    search.tm.hard_limit = hard_limit;

    let mut best_move: Option<Move> = None;
    let mut score = 0;

    for d in 1..=max_depth {
        search.seldepth = 0;
        let (mut alpha, mut beta) = if d > 3 {
            (score - 40, score + 40)
        } else {
            (-MATE_SCORE, MATE_SCORE)
        };

        loop {
            score = negamax(&mut search, alpha, beta, d as i32);

            if search.tm.check_hard_limit() {
                break;
            }

            if score <= alpha {
                alpha = -MATE_SCORE;
            } else if score >= beta {
                beta = MATE_SCORE;
            } else {
                break;
            }
        }

        if let Some(entry) = search.tt.probe(search.board.zobrist) {
            best_move = entry.best_move();
        }

        if is_main_thread {
            let elapsed_ms = search.tm.start_time.elapsed().as_millis();
            let nps = if elapsed_ms > 0 {
                (search.tm.nodes * 1000) / elapsed_ms as u64
            } else {
                0
            };
            let hashfull = search.tt.hashfull_permill();
            let pv = get_pv_from_tt(search.board.clone(), search.tt, d);
            let pv_str = pv
                .iter()
                .map(|&m| format_uci(m))
                .collect::<Vec<_>>()
                .join(" ");
            let score_str = if score.abs() > MATE_THRESHOLD {
                let mate_in = (MATE_SCORE - score.abs() + 1) / 2;
                format!("mate {}", if score > 0 { mate_in } else { -mate_in })
            } else {
                format!("cp {}", score)
            };
            println!(
                "info depth {} seldepth {} score {} hashfull {} nodes {} nps {} time {} pv {}",
                d, search.seldepth, score_str, hashfull, search.tm.nodes, nps, elapsed_ms, pv_str
            );
        }

        if search.tm.check_soft_limit(d, best_move) {
            break;
        }

        if score.abs() > MATE_THRESHOLD {
            break;
        }
    }

    (best_move, max_depth, search.tm.nodes)
}
