use crate::board::Board;
use crate::nnue::evaluate;
use crate::see::see;
use crate::time::TimeBudget;
use crate::tt::{Bound, SharedTransTable};
use crate::types::{Move, MoveList, Piece, PieceKind};
use crate::uci_io::format_uci;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

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

const SEARCH_THREAD_STACK: usize = 32 * 1024 * 1024;
const NO_DEADLINE: u64 = u64::MAX;
const TIME_CHECK_INTERVAL: u64 = 2048;
const NEW_ITERATION_FRACTION: f64 = 0.6;
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Shared between the UCI loop and all search threads of one `go`.
pub struct SearchControl {
    stop: AtomicBool,
    start: Instant,
    soft_deadline_ms: AtomicU64,
    hard_deadline_ms: AtomicU64,
}

impl SearchControl {
    pub fn new(budget: Option<TimeBudget>) -> Arc<Self> {
        let (soft, hard) = budget.map_or((NO_DEADLINE, NO_DEADLINE), |b| (b.soft_ms, b.hard_ms));
        Arc::new(Self {
            stop: AtomicBool::new(false),
            start: Instant::now(),
            soft_deadline_ms: AtomicU64::new(soft),
            hard_deadline_ms: AtomicU64::new(hard),
        })
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Starts the clock for a search that was running untimed (`ponderhit`).
    pub fn set_budget_from_now(&self, budget: TimeBudget) {
        let now = self.elapsed_ms();
        self.soft_deadline_ms
            .store(now + budget.soft_ms, Ordering::Relaxed);
        self.hard_deadline_ms
            .store(now + budget.hard_ms, Ordering::Relaxed);
    }

    fn is_timed(&self) -> bool {
        self.hard_deadline_ms.load(Ordering::Relaxed) != NO_DEADLINE
    }

    /// `go movetime`: use the whole budget instead of stopping between iterations.
    fn is_fixed_time(&self) -> bool {
        self.soft_deadline_ms() >= self.hard_deadline_ms()
    }

    fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn soft_deadline_ms(&self) -> u64 {
        self.soft_deadline_ms.load(Ordering::Relaxed)
    }

    fn hard_deadline_ms(&self) -> u64 {
        self.hard_deadline_ms.load(Ordering::Relaxed)
    }
}

#[derive(Copy, Clone)]
pub struct SearchLimits {
    pub max_depth: usize,
    /// `go infinite` / `go ponder`: never return before `stop` (or `ponderhit` sets a budget).
    pub wait_for_stop: bool,
}

pub struct SearchResult {
    pub best_move: Option<Move>,
    pub ponder_move: Option<Move>,
    pub nodes: u64,
}

struct TimeManager {
    control: Arc<SearchControl>,
    is_main_thread: bool,
    nodes: u64,
    best_move_stability: usize,
    previous_best_move: Option<Move>,
}

impl TimeManager {
    fn new(control: Arc<SearchControl>, is_main_thread: bool) -> Self {
        Self {
            control,
            is_main_thread,
            nodes: 0,
            best_move_stability: 0,
            previous_best_move: None,
        }
    }

    #[inline(always)]
    fn check_hard_limit(&mut self) -> bool {
        if self.control.is_stopped() {
            return true;
        }
        if self.is_main_thread
            && self.nodes % TIME_CHECK_INTERVAL == 0
            && self.control.elapsed_ms() >= self.control.hard_deadline_ms()
        {
            self.control.stop();
            return true;
        }
        false
    }

    /// Decides after a completed iteration whether another one is worth starting.
    fn should_stop_iterating(&mut self, depth: usize, best_move: Option<Move>) -> bool {
        if best_move == self.previous_best_move {
            self.best_move_stability += 1;
        } else {
            self.best_move_stability = 0;
        }
        self.previous_best_move = best_move;

        if !self.is_main_thread || !self.control.is_timed() || self.control.is_fixed_time() {
            return false;
        }

        let stability_scale = if self.best_move_stability == 0 {
            1.3
        } else if depth > 8 && self.best_move_stability >= 4 {
            0.7
        } else {
            1.0
        };
        let target = self.control.soft_deadline_ms() as f64 * stability_scale;
        let elapsed = self.control.elapsed_ms();
        elapsed as f64 >= target * NEW_ITERATION_FRACTION
            || elapsed >= self.control.hard_deadline_ms()
    }
}

struct Search<'a> {
    board: Board,
    tt: &'a SharedTransTable,
    tm: TimeManager,
    killers: [[Option<Move>; 2]; MAX_PLY],
    history: [[i32; 64]; 13],
    counter_moves: [[[Option<Move>; 64]; 13]; 2],
    ply: usize,
    seldepth: usize,
    prev_move: [Option<Move>; MAX_PLY],
    /// Best root move found so far; only moves that raised alpha in a finished subtree.
    root_best: Option<(Move, i32)>,
}

#[inline(always)]
fn side_to_move_in_check(board: &Board) -> bool {
    let king_sq = board.king_square(board.turn) as i32;
    board.is_square_attacked(king_sq, board.turn.other())
}

/// True when the side that just moved left its own king attacked.
#[inline(always)]
fn mover_left_king_in_check(board: &Board) -> bool {
    let mover = board.turn.other();
    let king_bb = board.piece_bb[Piece::from_kind(PieceKind::King, mover).index()];
    king_bb != 0 && board.is_square_attacked(king_bb.trailing_zeros() as i32, board.turn)
}

/// Piece-index for the counter-move table: the piece that made `prev` now stands on `prev.to`.
#[inline(always)]
fn counter_move_slot(board: &Board, prev: Move) -> (usize, usize, usize) {
    (
        prev.capture as usize,
        board.piece_on[prev.to as usize].index(),
        prev.to as usize,
    )
}

fn quiesce(s: &mut Search, mut alpha: i32, beta: i32) -> i32 {
    use crate::movepicker::QMovePicker;

    s.seldepth = s.seldepth.max(s.ply);
    s.tm.nodes += 1;

    if s.tm.check_hard_limit() {
        return 0;
    }
    if s.ply >= MAX_PLY - 1 {
        return evaluate(&s.board);
    }

    let in_check = side_to_move_in_check(&s.board);

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
            if mover_left_king_in_check(&s.board) {
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

    let mut picker = QMovePicker::new();
    while let Some(m) = picker.next(&s.board) {
        if m.capture && m.promotion.is_none() {
            let captured = s.board.piece_on[m.to as usize];
            if !captured.is_empty() {
                let piece_val = PIECE_VALUES[captured.kind().unwrap() as usize];
                if stand_pat + piece_val + DELTA_MARGIN < alpha {
                    continue;
                }
            }
        }
        if m.capture && see(&s.board, m) < 0 {
            continue;
        }

        let undo = s.board.make_move(m);
        if mover_left_king_in_check(&s.board) {
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

    if s.ply > 0 && (s.board.is_repetition(s.ply) || s.board.halfmove_clock >= 100) {
        return DRAW_SCORE;
    }
    if s.ply >= MAX_PLY - 1 {
        return evaluate(&s.board);
    }

    let is_root = s.ply == 0;
    let is_pv = beta - alpha > 1;
    let alpha_orig = alpha;
    let key = s.board.zobrist;
    let mut tt_move: Option<Move> = None;

    if let Some(entry) = s.tt.probe(key) {
        if entry.depth() >= depth as i16 && !is_root {
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

    let in_check = side_to_move_in_check(&s.board);

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
        s.prev_move[s.ply] = None;
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

    let killer1 = s.killers[s.ply][0];
    let killer2 = s.killers[s.ply][1];
    let previous_move = if is_root { None } else { s.prev_move[s.ply] };
    let counter_move = previous_move.and_then(|prev| {
        let (capture, piece, to) = counter_move_slot(&s.board, prev);
        s.counter_moves[capture][piece][to]
    });

    let mut picker =
        crate::movepicker::MovePicker::new(tt_move, killer1, killer2, counter_move);
    let mut best_score = -MATE_SCORE;
    let mut best_move: Option<Move> = None;
    let mut moves_searched = 0;
    let mut searched_quiets = [Move::default(); 128];
    let mut num_searched_quiets: usize = 0;

    while let Some(m) = picker.next(&s.board, &s.history) {
        if !is_pv && !in_check && depth <= 3 && !m.capture && m.promotion.is_none() {
            let lmp_limit = LMP_LIMITS[depth as usize];
            if moves_searched as i32 >= lmp_limit {
                continue;
            }
        }
        if depth <= 2 && !in_check && !m.capture && m.promotion.is_none() {
            let piece_idx = s.board.piece_on[m.from as usize].index();
            let hist_score = s.history[piece_idx][m.to as usize];
            if hist_score < -HISTORY_PRUNE_THRESHOLD {
                continue;
            }
        }
        if moves_searched > 0 && depth < 8 && !in_check && m.capture && see(&s.board, m) < 0 {
            continue;
        }

        let moved_piece = s.board.piece_on[m.from as usize];
        let undo = s.board.make_move(m);
        if mover_left_king_in_check(&s.board) {
            s.board.unmake_move(m, undo);
            continue;
        }
        s.tt.prefetch(s.board.zobrist);

        s.ply += 1;
        s.prev_move[s.ply] = Some(m);
        moves_searched += 1;

        let score;
        if moves_searched == 1 {
            score = -negamax(s, -beta, -alpha, depth - 1);
        } else {
            let mut reduction = 0;
            if depth >= 3 && !m.capture && !in_check {
                let d = depth as f32;
                let mn = moves_searched as f32;
                reduction = (0.5 + d.ln() * mn.ln() / 2.0) as i32;
                if !is_pv {
                    reduction += 1;
                }
                let history_score = s.history[moved_piece.index()][m.to as usize];
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

        if is_root && (s.root_best.is_none() || score > alpha) {
            s.root_best = Some((m, score));
        }

        if score > best_score {
            best_score = score;
            best_move = Some(m);
            if score > alpha {
                alpha = score;
                if alpha >= beta {
                    if !m.capture {
                        if Some(m) != s.killers[s.ply][0] {
                            s.killers[s.ply][1] = s.killers[s.ply][0];
                            s.killers[s.ply][0] = Some(m);
                        }

                        if let Some(prev) = previous_move {
                            let (capture, piece, to) = counter_move_slot(&s.board, prev);
                            s.counter_moves[capture][piece][to] = Some(m);
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

                        for failed_move in &searched_quiets[..num_searched_quiets] {
                            let p_idx = s.board.piece_on[failed_move.from as usize].index();
                            s.history[p_idx][failed_move.to as usize] -= bonus;
                        }
                    }
                    break;
                }
            }
        }

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
    s.tt
        .store(key, depth as i16, score_to_store, None, bound, best_move);
    best_score
}

/// Follows TT best moves from `pos`, stopping at the first move that is not legal there.
pub fn get_pv_from_tt(mut pos: Board, tt: &SharedTransTable, max_len: usize) -> Vec<Move> {
    let mut pv = Vec::with_capacity(max_len);
    for _ in 0..max_len {
        let Some(m) = tt.probe(pos.zobrist).and_then(|e| e.best_move()) else {
            break;
        };
        if !pos.is_pseudo_legal(m) {
            break;
        }
        let undo = pos.make_move(m);
        if mover_left_king_in_check(&pos) {
            pos.unmake_move(m, undo);
            break;
        }
        pv.push(m);
    }
    pv
}

fn format_score(score: i32) -> String {
    if score.abs() > MATE_THRESHOLD {
        let mate_in = (MATE_SCORE - score.abs() + 1) / 2;
        format!("mate {}", if score > 0 { mate_in } else { -mate_in })
    } else {
        format!("cp {}", score)
    }
}

fn print_iteration_info(s: &Search, depth: usize, best_move: Move, score: i32) {
    let elapsed_ms = s.tm.control.elapsed_ms();
    let nps = if elapsed_ms > 0 {
        s.tm.nodes * 1000 / elapsed_ms
    } else {
        0
    };
    let mut after_best = s.board.clone();
    after_best.make_move(best_move);
    let pv_str = std::iter::once(best_move)
        .chain(get_pv_from_tt(after_best, s.tt, depth.saturating_sub(1)))
        .map(format_uci)
        .collect::<Vec<_>>()
        .join(" ");
    println!(
        "info depth {} seldepth {} score {} hashfull {} nodes {} nps {} time {} pv {}",
        depth,
        s.seldepth,
        format_score(score),
        s.tt.hashfull_permill(),
        s.tm.nodes,
        nps,
        elapsed_ms,
        pv_str
    );
}

fn legal_moves(board: &Board) -> MoveList {
    let mut moves = MoveList::new();
    board.clone().generate_legal_moves(&mut moves);
    moves
}

/// Iterative deepening on one thread; returns the best root move and the nodes searched.
fn iterative_deepening(
    board: &Board,
    tt: &SharedTransTable,
    max_depth: usize,
    control: Arc<SearchControl>,
    is_main_thread: bool,
) -> (Option<Move>, u64) {
    let mut search = Search {
        board: board.clone(),
        tt,
        tm: TimeManager::new(control, is_main_thread),
        killers: [[None; 2]; MAX_PLY],
        history: [[0; 64]; 13],
        counter_moves: [[[None; 64]; 13]; 2],
        ply: 0,
        seldepth: 0,
        prev_move: [None; MAX_PLY],
        root_best: None,
    };

    let mut score = 0;
    for depth in 1..=max_depth {
        search.seldepth = 0;
        let (mut alpha, mut beta) = if depth > 3 {
            (score - 40, score + 40)
        } else {
            (-MATE_SCORE, MATE_SCORE)
        };

        let mut aborted = false;
        loop {
            let iteration_score = negamax(&mut search, alpha, beta, depth as i32);
            if search.tm.check_hard_limit() {
                aborted = true;
                break;
            }
            score = iteration_score;
            if score <= alpha {
                alpha = -MATE_SCORE;
            } else if score >= beta {
                beta = MATE_SCORE;
            } else {
                break;
            }
        }

        if aborted {
            break;
        }

        let best_move = search.root_best.map(|(m, _)| m);
        if is_main_thread && let Some(m) = best_move {
            print_iteration_info(&search, depth, m, score);
        }
        if search.tm.should_stop_iterating(depth, best_move) {
            break;
        }
        if score.abs() > MATE_THRESHOLD {
            break;
        }
    }

    (search.root_best.map(|(m, _)| m), search.tm.nodes)
}

fn ponder_move_after(board: &Board, best_move: Move, tt: &SharedTransTable) -> Option<Move> {
    let mut after_best = board.clone();
    after_best.make_move(best_move);
    get_pv_from_tt(after_best, tt, 1).first().copied()
}

fn wait_until_released(control: &SearchControl) {
    while !control.is_stopped() && !control.is_timed() {
        thread::sleep(STOP_POLL_INTERVAL);
    }
}

/// Runs a Lazy-SMP search with `threads` threads; thread 0 reports and manages time.
pub fn search(
    board: &Board,
    tt: &SharedTransTable,
    limits: SearchLimits,
    threads: usize,
    control: &Arc<SearchControl>,
) -> SearchResult {
    tt.tick_age();

    let root_moves = legal_moves(board);
    if root_moves.is_empty() {
        if limits.wait_for_stop {
            wait_until_released(control);
        }
        return SearchResult {
            best_move: None,
            ponder_move: None,
            nodes: 0,
        };
    }
    if root_moves.len() == 1 && control.is_timed() && !limits.wait_for_stop {
        return SearchResult {
            best_move: Some(root_moves.moves[0]),
            ponder_move: None,
            nodes: 0,
        };
    }

    let (best_move, nodes) = thread::scope(|scope| {
        let helpers: Vec<_> = (1..threads.max(1))
            .filter_map(|i| {
                let helper_control = Arc::clone(control);
                thread::Builder::new()
                    .name(format!("search-helper-{i}"))
                    .stack_size(SEARCH_THREAD_STACK)
                    .spawn_scoped(scope, move || {
                        iterative_deepening(board, tt, limits.max_depth, helper_control, false).1
                    })
                    .ok()
            })
            .collect();

        let main_control = Arc::clone(control);
        let main = thread::Builder::new()
            .name("search-main".into())
            .stack_size(SEARCH_THREAD_STACK)
            .spawn_scoped(scope, move || {
                let result =
                    iterative_deepening(board, tt, limits.max_depth, Arc::clone(&main_control), true);
                if limits.wait_for_stop {
                    wait_until_released(&main_control);
                }
                main_control.stop();
                result
            });

        let (best_move, main_nodes) = match main {
            Ok(handle) => handle.join().unwrap_or((None, 0)),
            Err(_) => {
                control.stop();
                (None, 0)
            }
        };
        let helper_nodes: u64 = helpers.into_iter().map(|h| h.join().unwrap_or(0)).sum();
        (best_move, main_nodes + helper_nodes)
    });

    let best_move = best_move.or(Some(root_moves.moves[0]));
    let ponder_move = best_move.and_then(|m| ponder_move_after(board, m, tt));
    SearchResult {
        best_move,
        ponder_move,
        nodes,
    }
}
