use crate::board::Board;
use crate::nnue::evaluate;
use crate::legality::LegalityInfo;
use crate::history::{ContinuationKey, Histories, captured_kind, stat_bonus};
use crate::movepicker::{MovePicker, OrderingContext};
use crate::see::{PIECE_VALUES, see, see_ge};
use crate::time::TimeBudget;
use crate::tt::{Bound, SharedTransTable};
use crate::types::{MAX_MOVES, Move, MoveList, Piece, StackVec, Undo};
use crate::uci_io::format_uci;
use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

pub const MATE_SCORE: i32 = 30_000;
const MATE_THRESHOLD: i32 = MATE_SCORE - 512;
const INFINITY: i32 = MATE_SCORE + 1;
const MAX_PLY: usize = 128;
const DRAW_SCORE: i32 = 0;

const RFP_MAX_DEPTH: i32 = 8;
const RFP_MARGIN: i32 = 80;
const RAZORING_MARGIN: i32 = 400;
const RAZORING_DEPTH_SQ_MARGIN: i32 = 250;
const NMP_MIN_DEPTH: i32 = 3;
const NMP_EVAL_DIVISOR: i32 = 200;
const IIR_MIN_DEPTH: i32 = 4;
const MOVE_PRUNING_MAX_DEPTH: i32 = 8;
const FUTILITY_BASE: i32 = 100;
const FUTILITY_PER_DEPTH: i32 = 100;
const SEE_QUIET_MARGIN: i32 = -50;
const SEE_CAPTURE_MARGIN: i32 = -90;
const DELTA_MARGIN: i32 = 200;
const ASPIRATION_DELTA: i32 = 25;
const ASPIRATION_MIN_DEPTH: usize = 4;
const SINGULAR_MIN_DEPTH: i32 = 8;
const SINGULAR_TT_DEPTH_MARGIN: i32 = 3;
const SINGULAR_BETA_PER_DEPTH: i32 = 2;
const DOUBLE_EXTENSION_MARGIN: i32 = 20;
const MAX_DOUBLE_EXTENSIONS: u8 = 6;

const MAX_TRIED_MOVES: usize = 64;

const LMR_MIN_DEPTH: i32 = 3;
const LMR_TABLE_SIZE: usize = 64;
const LMR_QUIET_BASE: f64 = 0.77;
const LMR_QUIET_DIVISOR: f64 = 2.36;
const LMR_CAPTURE_BASE: f64 = 0.0;
const LMR_CAPTURE_DIVISOR: f64 = 3.0;
const LMR_HISTORY_DIVISOR: i32 = 8192;
const DEEPER_MARGIN: i32 = 70;
const SHALLOWER_MARGIN: i32 = 8;

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

/// Move-ordering statistics one search thread keeps between `go` commands.
pub struct ThreadData {
    histories: Histories,
    counter_moves: [[[Option<Move>; 64]; 13]; 2],
}

impl ThreadData {
    fn new() -> Box<Self> {
        Box::new(Self {
            histories: Histories::new(),
            counter_moves: [[[None; 64]; 13]; 2],
        })
    }
}

/// Per-thread state for a Lazy-SMP search; thread 0 is the main thread.
pub struct SearchThreads {
    data: Vec<Box<ThreadData>>,
}

impl SearchThreads {
    pub fn new(count: usize) -> Self {
        Self {
            data: (0..count.max(1)).map(|_| ThreadData::new()).collect(),
        }
    }

    pub fn count(&self) -> usize {
        self.data.len()
    }

    /// Forgets everything learned in previous games (`ucinewgame`).
    pub fn clear(&mut self) {
        let count = self.count();
        *self = Self::new(count);
    }
}

struct LmrTables {
    quiet: [[i32; LMR_TABLE_SIZE]; LMR_TABLE_SIZE],
    capture: [[i32; LMR_TABLE_SIZE]; LMR_TABLE_SIZE],
}

static LMR_TABLES: OnceLock<LmrTables> = OnceLock::new();

fn lmr_tables() -> &'static LmrTables {
    LMR_TABLES.get_or_init(|| {
        let mut tables = LmrTables {
            quiet: [[0; LMR_TABLE_SIZE]; LMR_TABLE_SIZE],
            capture: [[0; LMR_TABLE_SIZE]; LMR_TABLE_SIZE],
        };
        for depth in 1..LMR_TABLE_SIZE {
            for moves in 1..LMR_TABLE_SIZE {
                let product = (depth as f64).ln() * (moves as f64).ln();
                tables.quiet[depth][moves] = (LMR_QUIET_BASE + product / LMR_QUIET_DIVISOR) as i32;
                tables.capture[depth][moves] =
                    (LMR_CAPTURE_BASE + product / LMR_CAPTURE_DIVISOR) as i32;
            }
        }
        tables
    })
}

/// Base late-move reduction for the `moves_searched`-th move at `depth`.
fn base_reduction(depth: i32, moves_searched: i32, is_quiet: bool) -> i32 {
    let d = (depth.max(0) as usize).min(LMR_TABLE_SIZE - 1);
    let m = (moves_searched.max(0) as usize).min(LMR_TABLE_SIZE - 1);
    let tables = lmr_tables();
    if is_quiet {
        tables.quiet[d][m]
    } else {
        tables.capture[d][m]
    }
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

/// What the search knows about one ply of the current line.
#[derive(Copy, Clone)]
struct StackEntry {
    static_eval: Option<i32>,
    /// Move played from this ply (`None` for a null move or before one is made).
    current_move: Option<Move>,
    /// Piece that made `current_move`.
    moved_piece: Piece,
    /// Move skipped by a singular-extension verification search at this ply.
    excluded_move: Option<Move>,
    /// Double extensions applied on the path to this ply's children.
    double_extensions: u8,
}

impl Default for StackEntry {
    fn default() -> Self {
        Self {
            static_eval: None,
            current_move: None,
            moved_piece: Piece::Empty,
            excluded_move: None,
            double_extensions: 0,
        }
    }
}

struct Search<'a> {
    board: Board,
    tt: &'a SharedTransTable,
    td: &'a mut ThreadData,
    tm: TimeManager,
    killers: [[Option<Move>; 2]; MAX_PLY],
    stack: [StackEntry; MAX_PLY + 1],
    ply: usize,
    seldepth: usize,
    /// Best root move found so far; only moves that raised alpha in a finished subtree.
    root_best: Option<(Move, i32)>,
}

#[inline(always)]
fn side_to_move_in_check(board: &Board) -> bool {
    let king_sq = board.king_square(board.turn) as i32;
    board.is_square_attacked(king_sq, board.turn.other())
}

/// Makes `m` if it is legal in `pos`.
fn make_if_legal(pos: &mut Board, m: Move) -> Option<Undo> {
    let info = pos.legality_info()?;
    (pos.is_pseudo_legal(m) && pos.is_legal(m, &info)).then(|| pos.make_move(m))
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

/// Mate scores are stored relative to the node, not the root.
#[inline(always)]
fn score_to_tt(score: i32, ply: usize) -> i32 {
    if score > MATE_THRESHOLD {
        score + ply as i32
    } else if score < -MATE_THRESHOLD {
        score - ply as i32
    } else {
        score
    }
}

#[inline(always)]
fn score_from_tt(score: i32, ply: usize) -> i32 {
    if score > MATE_THRESHOLD {
        score - ply as i32
    } else if score < -MATE_THRESHOLD {
        score + ply as i32
    } else {
        score
    }
}

#[inline(always)]
fn tt_cutoff_allowed(bound: Bound, score: i32, alpha: i32, beta: i32) -> bool {
    match bound {
        Bound::Exact => true,
        Bound::Lower => score >= beta,
        Bound::Upper => score <= alpha,
    }
}

/// Whether a TT score is a tighter estimate of the position than the static eval.
#[inline(always)]
fn tt_score_refines_eval(bound: Bound, tt_score: i32, eval: i32) -> bool {
    match bound {
        Bound::Exact => true,
        Bound::Lower => tt_score > eval,
        Bound::Upper => tt_score < eval,
    }
}

#[inline(always)]
fn bound_for(best_score: i32, alpha_orig: i32, beta: i32) -> Bound {
    if best_score <= alpha_orig {
        Bound::Upper
    } else if best_score >= beta {
        Bound::Lower
    } else {
        Bound::Exact
    }
}

fn has_non_pawn_material(board: &Board) -> bool {
    let our_pieces = if board.turn == crate::types::Color::White {
        board.w_pieces
    } else {
        board.b_pieces
    };
    let pawns_and_kings = board.piece_bb[Piece::WP.index()]
        | board.piece_bb[Piece::BP.index()]
        | board.piece_bb[Piece::WK.index()]
        | board.piece_bb[Piece::BK.index()];
    our_pieces & !pawns_and_kings != 0
}

impl Search<'_> {
    /// Static eval is better than two plies ago (four if that node was in check).
    fn is_improving(&self, static_eval: Option<i32>) -> bool {
        let Some(eval) = static_eval else {
            return false;
        };
        let earlier = |back: usize| {
            self.ply
                .checked_sub(back)
                .map(|ply| self.stack[ply].static_eval)
        };
        match earlier(2) {
            Some(Some(previous)) => eval > previous,
            Some(None) => match earlier(4) {
                Some(Some(previous)) => eval > previous,
                _ => true,
            },
            None => true,
        }
    }

    fn previous_move(&self) -> Option<Move> {
        if self.ply == 0 {
            None
        } else {
            self.stack[self.ply - 1].current_move
        }
    }

    /// Static eval for the current node, reusing the TT's copy when it has one.
    fn static_eval(&mut self, tt_eval: Option<i32>) -> i32 {
        tt_eval.unwrap_or_else(|| evaluate(&mut self.board))
    }

    /// Makes `m` (pseudo-legal in the current position) unless it leaves the king in check.
    fn make_move(&mut self, m: Move, legality: &LegalityInfo) -> Option<Undo> {
        if !self.board.is_legal(m, legality) {
            return None;
        }
        let moved_piece = self.board.piece_on[m.from as usize];
        let undo = self.board.make_move(m);
        self.tt.prefetch(self.board.zobrist);
        self.stack[self.ply].current_move = Some(m);
        self.stack[self.ply].moved_piece = moved_piece;
        self.ply += 1;
        Some(undo)
    }

    fn make_null_move(&mut self) -> Undo {
        let undo = self.board.make_null_move();
        self.stack[self.ply].current_move = None;
        self.stack[self.ply].moved_piece = Piece::Empty;
        self.ply += 1;
        undo
    }

    fn unmake_null_move(&mut self, undo: Undo) {
        self.ply -= 1;
        self.board.unmake_null_move(undo);
    }

    /// Continuation-history keys for the moves played one and two plies ago.
    fn continuation_keys(&self) -> [Option<ContinuationKey>; 2] {
        let key = |back: usize| {
            let entry = self.stack[self.ply.checked_sub(back)?];
            entry
                .current_move
                .map(|m| ContinuationKey::new(entry.moved_piece, m.to))
        };
        [key(1), key(2)]
    }

    fn unmake_move(&mut self, m: Move, undo: Undo) {
        self.ply -= 1;
        self.board.unmake_move(m, undo);
    }

    fn update_capture_history(&mut self, m: Move, bonus: i32) {
        let piece = self.board.piece_on[m.from as usize];
        if let Some(victim) = captured_kind(self.board.piece_on[m.to as usize], m) {
            self.td.histories.update_capture(piece, m.to, victim, bonus);
        }
    }

    /// Rewards the move that caused a beta cutoff and penalizes the moves tried before it.
    fn update_cutoff_stats(
        &mut self,
        best: Move,
        depth: i32,
        tried_quiets: &[Move],
        tried_captures: &[Move],
    ) {
        let bonus = stat_bonus(depth);
        let continuations = self.continuation_keys();
        let color = self.board.turn;

        if best.capture {
            self.update_capture_history(best, bonus);
        } else {
            let ply = self.ply;
            if Some(best) != self.killers[ply][0] {
                self.killers[ply][1] = self.killers[ply][0];
                self.killers[ply][0] = Some(best);
            }
            if let Some(prev) = self.previous_move() {
                let (capture, piece, to) = counter_move_slot(&self.board, prev);
                self.td.counter_moves[capture][piece][to] = Some(best);
            }
            let piece = self.board.piece_on[best.from as usize];
            self.td
                .histories
                .update_quiet(color, best, piece, &continuations, bonus);
            for &quiet in tried_quiets {
                let piece = self.board.piece_on[quiet.from as usize];
                self.td
                    .histories
                    .update_quiet(color, quiet, piece, &continuations, -bonus);
            }
        }

        for &capture in tried_captures {
            self.update_capture_history(capture, -bonus);
        }
    }
}

fn quiesce(s: &mut Search, mut alpha: i32, beta: i32) -> i32 {
    use crate::movepicker::QMovePicker;

    s.seldepth = s.seldepth.max(s.ply);
    s.tm.nodes += 1;

    if s.tm.check_hard_limit() {
        return 0;
    }
    if s.ply >= MAX_PLY - 1 {
        return evaluate(&mut s.board);
    }

    let is_pv = beta - alpha > 1;
    let alpha_orig = alpha;
    let key = s.board.zobrist;
    let tt_entry = s.tt.probe(key);
    if let Some(entry) = tt_entry {
        let tt_score = score_from_tt(entry.score(), s.ply);
        if !is_pv && tt_cutoff_allowed(entry.bound(), tt_score, alpha, beta) {
            return tt_score;
        }
    }

    let Some(legality) = s.board.legality_info() else {
        // Unreachable from a parsed FEN: every position has both kings.
        return DRAW_SCORE;
    };
    if legality.in_check() {
        return quiesce_evasions(s, alpha, beta, &legality);
    }

    let raw_eval = s.static_eval(tt_entry.and_then(|e| e.eval()));
    let stand_pat = match tt_entry {
        Some(entry)
            if tt_score_refines_eval(entry.bound(), score_from_tt(entry.score(), s.ply), raw_eval) =>
        {
            score_from_tt(entry.score(), s.ply)
        }
        _ => raw_eval,
    };
    if stand_pat >= beta {
        if tt_entry.is_none() {
            s.tt.store(key, 0, score_to_tt(stand_pat, s.ply), Some(raw_eval), Bound::Lower, None);
        }
        return stand_pat;
    }
    alpha = alpha.max(stand_pat);

    let mut best_score = stand_pat;
    let mut best_move = None;
    let mut picker = QMovePicker::new();
    while let Some(m) = picker.next(&s.board) {
        if m.capture && m.promotion.is_none() {
            let captured = s.board.piece_on[m.to as usize];
            if let Some(kind) = captured.kind()
                && stand_pat + PIECE_VALUES[kind as usize] + DELTA_MARGIN < alpha
            {
                continue;
            }
        }
        if m.capture && !see_ge(&s.board, m, 0) {
            continue;
        }

        let Some(undo) = s.make_move(m, &legality) else {
            continue;
        };
        let score = -quiesce(s, -beta, -alpha);
        s.unmake_move(m, undo);

        if s.tm.check_hard_limit() {
            return 0;
        }

        if score > best_score {
            best_score = score;
            if score > alpha {
                best_move = Some(m);
                alpha = score;
                if alpha >= beta {
                    break;
                }
            }
        }
    }

    let bound = bound_for(best_score, alpha_orig, beta);
    s.tt.store(key, 0, score_to_tt(best_score, s.ply), Some(raw_eval), bound, best_move);
    best_score
}

/// Quiescence when in check: every evasion is searched so mates are found.
fn quiesce_evasions(s: &mut Search, mut alpha: i32, beta: i32, legality: &LegalityInfo) -> i32 {
    let mut move_list = MoveList::new();
    s.board.generate_pseudo_legal_moves(&mut move_list);

    let mut scores = [0i32; MAX_MOVES];
    for (score, &m) in scores.iter_mut().zip(move_list.as_slice()) {
        *score = if m.capture { see(&s.board, m) } else { 0 };
    }

    let mut best_score = -MATE_SCORE + s.ply as i32;
    for i in 0..move_list.len() {
        let mut best_idx = i;
        for j in (i + 1)..move_list.len() {
            if scores[j] > scores[best_idx] {
                best_idx = j;
            }
        }
        move_list.swap(i, best_idx);
        scores.swap(i, best_idx);

        let m = move_list.get(i);
        let Some(undo) = s.make_move(m, legality) else {
            continue;
        };
        let score = -quiesce(s, -beta, -alpha);
        s.unmake_move(m, undo);

        if s.tm.check_hard_limit() {
            return 0;
        }

        if score > best_score {
            best_score = score;
            if score > alpha {
                alpha = score;
                if alpha >= beta {
                    break;
                }
            }
        }
    }
    best_score
}

enum Singularity {
    /// Search the move this many plies deeper (negative: shallower).
    Extend(i32),
    /// Even without the TT move the node fails high; return this score.
    MultiCut(i32),
    Aborted,
}

/// Verifies whether the TT move is singular by searching the node without it.
fn singular_extension(
    s: &mut Search,
    tt_move: Move,
    tt_entry: Option<crate::tt::TTEntry>,
    depth: i32,
    beta: i32,
    is_pv: bool,
    cut_node: bool,
) -> Singularity {
    let Some(entry) = tt_entry else {
        return Singularity::Extend(0);
    };
    let tt_score = score_from_tt(entry.score(), s.ply);
    let reliable = depth >= SINGULAR_MIN_DEPTH
        && i32::from(entry.depth()) >= depth - SINGULAR_TT_DEPTH_MARGIN
        && entry.bound() != Bound::Upper
        && tt_score.abs() < MATE_THRESHOLD;
    if !reliable {
        return Singularity::Extend(0);
    }

    let singular_beta = tt_score - SINGULAR_BETA_PER_DEPTH * depth;
    let singular_depth = (depth - 1) / 2;
    s.stack[s.ply].excluded_move = Some(tt_move);
    let singular_score = negamax(s, singular_beta - 1, singular_beta, singular_depth, cut_node);
    s.stack[s.ply].excluded_move = None;
    if s.tm.check_hard_limit() {
        return Singularity::Aborted;
    }

    if singular_score < singular_beta {
        let double = !is_pv
            && singular_score < singular_beta - DOUBLE_EXTENSION_MARGIN
            && s.stack[s.ply].double_extensions < MAX_DOUBLE_EXTENSIONS;
        Singularity::Extend(if double { 2 } else { 1 })
    } else if singular_beta >= beta {
        Singularity::MultiCut(singular_beta)
    } else if tt_score >= beta {
        Singularity::Extend(-1)
    } else {
        Singularity::Extend(0)
    }
}

/// `cut_node`: a non-PV node expected to fail high.
fn negamax(s: &mut Search, mut alpha: i32, mut beta: i32, mut depth: i32, cut_node: bool) -> i32 {
    s.seldepth = s.seldepth.max(s.ply);
    if s.tm.check_hard_limit() {
        return 0;
    }

    let is_root = s.ply == 0;
    if !is_root {
        if s.board.is_repetition(s.ply) || s.board.halfmove_clock >= 100 {
            return DRAW_SCORE;
        }
        if s.ply >= MAX_PLY - 1 {
            return evaluate(&mut s.board);
        }
        alpha = alpha.max(-MATE_SCORE + s.ply as i32);
        beta = beta.min(MATE_SCORE - s.ply as i32 - 1);
        if alpha >= beta {
            return alpha;
        }
    }

    let is_pv = beta - alpha > 1;
    let alpha_orig = alpha;
    let key = s.board.zobrist;
    let excluded_move = s.stack[s.ply].excluded_move;
    if excluded_move.is_none() {
        s.stack[s.ply].double_extensions = if is_root {
            0
        } else {
            s.stack[s.ply - 1].double_extensions
        };
    }

    let tt_entry = s.tt.probe(key);
    let tt_move = tt_entry.and_then(|e| e.best_move());
    if let Some(entry) = tt_entry {
        let tt_score = score_from_tt(entry.score(), s.ply);
        if !is_pv
            && excluded_move.is_none()
            && i32::from(entry.depth()) >= depth
            && tt_cutoff_allowed(entry.bound(), tt_score, alpha, beta)
        {
            return tt_score;
        }
    }

    let Some(legality) = s.board.legality_info() else {
        // Unreachable from a parsed FEN: every position has both kings.
        return DRAW_SCORE;
    };
    let in_check = legality.in_check();
    if in_check {
        depth += 1;
    }
    if depth <= 0 {
        return quiesce(s, alpha, beta);
    }

    s.tm.nodes += 1;
    s.killers[(s.ply + 1).min(MAX_PLY - 1)] = [None; 2];

    let raw_eval = if in_check {
        None
    } else {
        Some(s.static_eval(tt_entry.and_then(|e| e.eval())))
    };
    s.stack[s.ply].static_eval = raw_eval;
    let eval = match (raw_eval, tt_entry) {
        (Some(raw), Some(entry))
            if tt_score_refines_eval(entry.bound(), score_from_tt(entry.score(), s.ply), raw) =>
        {
            Some(score_from_tt(entry.score(), s.ply))
        }
        _ => raw_eval,
    };

    let improving = s.is_improving(raw_eval);

    if !is_pv && let Some(eval) = eval {
        if eval < alpha - RAZORING_MARGIN - RAZORING_DEPTH_SQ_MARGIN * depth * depth {
            let q_score = quiesce(s, alpha, beta);
            if q_score < alpha {
                return q_score;
            }
        }

        if depth <= RFP_MAX_DEPTH && eval - RFP_MARGIN * (depth - i32::from(improving)) >= beta {
            return eval;
        }

        let previous_was_null = !is_root && s.stack[s.ply - 1].current_move.is_none();
        if depth >= NMP_MIN_DEPTH
            && excluded_move.is_none()
            && eval >= beta
            && !previous_was_null
            && has_non_pawn_material(&s.board)
        {
            let r = 4 + depth / 3 + ((eval - beta) / NMP_EVAL_DIVISOR).min(3);
            let undo = s.make_null_move();
            let null_score = -negamax(s, -beta, -beta + 1, depth - r, !cut_node);
            s.unmake_null_move(undo);
            if s.tm.check_hard_limit() {
                return 0;
            }
            if null_score >= beta {
                return if null_score > MATE_THRESHOLD { beta } else { null_score };
            }
        }
    }

    if depth >= IIR_MIN_DEPTH && tt_move.is_none() && excluded_move.is_none() {
        depth -= 1;
    }

    let killer1 = s.killers[s.ply][0];
    let killer2 = s.killers[s.ply][1];
    let previous_move = s.previous_move();
    let counter_move = previous_move.and_then(|prev| {
        let (capture, piece, to) = counter_move_slot(&s.board, prev);
        s.td.counter_moves[capture][piece][to]
    });

    let mut picker = MovePicker::new(tt_move, killer1, killer2, counter_move);
    let continuations = s.continuation_keys();
    let mut best_score = -INFINITY;
    let mut best_move: Option<Move> = None;
    let mut moves_searched = 0;
    let mut tried_quiets: StackVec<Move, MAX_TRIED_MOVES> = StackVec::default();
    let mut tried_captures: StackVec<Move, MAX_TRIED_MOVES> = StackVec::default();

    let lmp_limit = (3 + depth * depth) / (2 - i32::from(improving));
    let mut quiets_seen = 0;

    loop {
        let ordering = OrderingContext {
            histories: &s.td.histories,
            continuations,
        };
        let Some(m) = picker.next(&s.board, &ordering) else {
            break;
        };
        if Some(m) == excluded_move {
            continue;
        }
        let is_quiet = !m.capture && m.promotion.is_none();
        if is_quiet {
            quiets_seen += 1;
        }

        let can_prune = !is_root && !in_check && moves_searched > 0 && best_score > -MATE_THRESHOLD;
        if can_prune && let Some(eval) = eval {
            if is_quiet {
                if quiets_seen > lmp_limit {
                    picker.skip_quiets();
                    continue;
                }
                if depth <= MOVE_PRUNING_MAX_DEPTH
                    && eval + FUTILITY_BASE + FUTILITY_PER_DEPTH * depth <= alpha
                {
                    picker.skip_quiets();
                    continue;
                }
                if depth <= MOVE_PRUNING_MAX_DEPTH
                    && !see_ge(&s.board, m, SEE_QUIET_MARGIN * depth * depth)
                {
                    continue;
                }
            } else if depth <= MOVE_PRUNING_MAX_DEPTH
                && !see_ge(&s.board, m, SEE_CAPTURE_MARGIN * depth)
            {
                continue;
            }
        }

        let quiet_history = if is_quiet {
            let piece = s.board.piece_on[m.from as usize];
            s.td.histories.butterfly(s.board.turn, m)
                + continuations
                    .iter()
                    .flatten()
                    .map(|&key| s.td.histories.continuation(key, piece, m.to))
                    .sum::<i32>()
        } else {
            0
        };
        let is_killer_or_counter =
            Some(m) == killer1 || Some(m) == killer2 || Some(m) == counter_move;

        let extension = if Some(m) == tt_move && !is_root && excluded_move.is_none() {
            match singular_extension(s, m, tt_entry, depth, beta, is_pv, cut_node) {
                Singularity::Extend(plies) => plies,
                Singularity::MultiCut(score) => return score,
                Singularity::Aborted => return 0,
            }
        } else {
            0
        };
        let node_ply = s.ply;
        let Some(undo) = s.make_move(m, &legality) else {
            continue;
        };
        moves_searched += 1;
        let new_depth = depth - 1 + extension;
        if extension >= 2 {
            s.stack[node_ply].double_extensions += 1;
        }

        let score = if moves_searched == 1 {
            -negamax(s, -beta, -alpha, new_depth, !is_pv && !cut_node)
        } else {
            let reduction = if depth >= LMR_MIN_DEPTH && !in_check && (is_quiet || !is_pv) {
                let gives_check = side_to_move_in_check(&s.board);
                let mut r = base_reduction(depth, moves_searched, is_quiet);
                r += i32::from(!is_pv);
                r += i32::from(cut_node);
                r += i32::from(!improving);
                r -= i32::from(is_killer_or_counter);
                r -= i32::from(gives_check);
                r -= quiet_history / LMR_HISTORY_DIVISOR;
                r.clamp(0, (new_depth - 1).max(0))
            } else {
                0
            };
            let reduced_depth = new_depth - reduction;

            let mut search_score = -negamax(s, -alpha - 1, -alpha, reduced_depth, true);
            if search_score > alpha && reduction > 0 {
                let deeper = search_score > best_score + DEEPER_MARGIN + 2 * new_depth;
                let shallower = search_score < best_score + SHALLOWER_MARGIN;
                let research_depth = new_depth + i32::from(deeper) - i32::from(shallower);
                if research_depth > reduced_depth {
                    search_score = -negamax(s, -alpha - 1, -alpha, research_depth, !cut_node);
                }
            }
            if is_pv && search_score > alpha && search_score < beta {
                search_score = -negamax(s, -beta, -alpha, new_depth, false);
            }
            search_score
        };

        s.unmake_move(m, undo);
        if extension >= 2 {
            s.stack[node_ply].double_extensions -= 1;
        }

        if s.tm.check_hard_limit() {
            return 0;
        }

        if is_root && (s.root_best.is_none() || score > alpha) {
            s.root_best = Some((m, score));
        }

        if score > best_score {
            best_score = score;
            if score > alpha {
                best_move = Some(m);
                alpha = score;
                if alpha >= beta {
                    s.update_cutoff_stats(m, depth, tried_quiets.as_slice(), tried_captures.as_slice());
                    break;
                }
            }
        }

        if is_quiet && tried_quiets.len() < MAX_TRIED_MOVES {
            tried_quiets.push(m);
        } else if m.capture && tried_captures.len() < MAX_TRIED_MOVES {
            tried_captures.push(m);
        }
    }

    if moves_searched == 0 {
        return if excluded_move.is_some() {
            alpha
        } else if in_check {
            -MATE_SCORE + s.ply as i32
        } else {
            DRAW_SCORE
        };
    }
    if excluded_move.is_some() {
        return best_score;
    }

    let bound = bound_for(best_score, alpha_orig, beta);
    s.tt.store(
        key,
        depth as i16,
        score_to_tt(best_score, s.ply),
        raw_eval,
        bound,
        best_move,
    );
    best_score
}

/// Follows TT best moves from `pos`, stopping at the first move that is not legal there.
pub fn get_pv_from_tt(mut pos: Board, tt: &SharedTransTable, max_len: usize) -> Vec<Move> {
    let mut pv = Vec::with_capacity(max_len);
    for _ in 0..max_len {
        let Some(m) = tt.probe(pos.zobrist).and_then(|e| e.best_move()) else {
            break;
        };
        if make_if_legal(&mut pos, m).is_none() {
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
    td: &mut ThreadData,
    max_depth: usize,
    control: Arc<SearchControl>,
    is_main_thread: bool,
) -> (Option<Move>, u64) {
    let mut search = Search {
        board: board.clone(),
        tt,
        td,
        tm: TimeManager::new(control, is_main_thread),
        killers: [[None; 2]; MAX_PLY],
        stack: [StackEntry::default(); MAX_PLY + 1],
        ply: 0,
        seldepth: 0,
        root_best: None,
    };

    let mut score = 0;
    for depth in 1..=max_depth {
        search.seldepth = 0;
        let mut delta = ASPIRATION_DELTA;
        let (mut alpha, mut beta) = if depth >= ASPIRATION_MIN_DEPTH {
            ((score - delta).max(-INFINITY), (score + delta).min(INFINITY))
        } else {
            (-INFINITY, INFINITY)
        };

        let mut aborted = false;
        let mut fail_highs = 0;
        loop {
            let search_depth = (depth as i32 - fail_highs).max(1);
            let iteration_score = negamax(&mut search, alpha, beta, search_depth, false);
            if search.tm.check_hard_limit() {
                aborted = true;
                break;
            }
            score = iteration_score;
            if score <= alpha {
                beta = (alpha + beta) / 2;
                alpha = (score - delta).max(-INFINITY);
                fail_highs = 0;
            } else if score >= beta {
                beta = (score + delta).min(INFINITY);
                fail_highs += 1;
            } else {
                break;
            }
            delta += delta / 2;
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

/// Runs a Lazy-SMP search with one thread per entry of `threads`; thread 0 reports
/// and manages time.
pub fn search(
    board: &Board,
    tt: &SharedTransTable,
    limits: SearchLimits,
    threads: &mut SearchThreads,
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
            best_move: Some(root_moves.get(0)),
            ponder_move: None,
            nodes: 0,
        };
    }

    let (main_td, helper_tds) = threads.data.split_at_mut(1);
    let (best_move, nodes) = thread::scope(|scope| {
        let helpers: Vec<_> = helper_tds
            .iter_mut()
            .enumerate()
            .filter_map(|(i, td)| {
                let helper_control = Arc::clone(control);
                thread::Builder::new()
                    .name(format!("search-helper-{}", i + 1))
                    .stack_size(SEARCH_THREAD_STACK)
                    .spawn_scoped(scope, move || {
                        iterative_deepening(board, tt, td, limits.max_depth, helper_control, false)
                            .1
                    })
                    .ok()
            })
            .collect();

        let main_control = Arc::clone(control);
        let main_td = &mut main_td[0];
        let main = thread::Builder::new()
            .name("search-main".into())
            .stack_size(SEARCH_THREAD_STACK)
            .spawn_scoped(scope, move || {
                let result = iterative_deepening(
                    board,
                    tt,
                    main_td,
                    limits.max_depth,
                    Arc::clone(&main_control),
                    true,
                );
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

    let best_move = best_move.or(Some(root_moves.get(0)));
    let ponder_move = best_move.and_then(|m| ponder_move_after(board, m, tt));
    SearchResult {
        best_move,
        ponder_move,
        nodes,
    }
}
