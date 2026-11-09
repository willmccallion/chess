use crate::board::Board;
use crate::eval::evaluate;
use crate::history::{ContinuationKey, CorrectionHistory, Histories, captured_kind, stat_bonus};
use crate::legality::LegalityInfo;
use crate::movepicker::{MovePicker, OrderingContext, ScoredMoves};
use crate::search_params::SearchParams;
use crate::see::{PIECE_VALUES, see, see_ge};
use crate::time::TimeBudget;
use crate::tt::{Bound, SharedTransTable, TTEntry, TTWrite};
use crate::types::{Move, MoveList, Piece, StackVec, Undo, ZKey};
use crate::uci_io::format_uci;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

pub const MATE_SCORE: i32 = 30_000;
const MATE_THRESHOLD: i32 = MATE_SCORE - 512;
/// Past every score a search can return, mates included: the widest window.
const SCORE_BOUND: i32 = MATE_SCORE + 1;
const MAX_PLY: usize = 128;
const DRAW_SCORE: i32 = 0;

const ASPIRATION_MIN_DEPTH: usize = 4;
const MAX_DOUBLE_EXTENSIONS: u8 = 6;

const MAX_TRIED_MOVES: usize = 64;

const LMR_TABLE_SIZE: usize = 64;

/// The search measures scores in units of 3/4 centipawn: its margins were set for an
/// evaluation about 4/3 the size of Stockfish 18's centipawns.
const SEARCH_UNITS_PER_CP_NUM: i32 = 4;
const SEARCH_UNITS_PER_CP_DEN: i32 = 3;

const fn cp_to_search_units(cp: i32) -> i32 {
    cp * SEARCH_UNITS_PER_CP_NUM / SEARCH_UNITS_PER_CP_DEN
}

const fn search_units_to_cp(score: i32) -> i32 {
    score * SEARCH_UNITS_PER_CP_DEN / SEARCH_UNITS_PER_CP_NUM
}

const SEARCH_THREAD_STACK: usize = 32 * 1024 * 1024;
const NO_DEADLINE: u64 = u64::MAX;
const TIME_CHECK_INTERVAL: u64 = 2048;
const NEW_ITERATION_FRACTION: f64 = 0.6;
const NODE_SHARE_PIVOT: f64 = 1.6;
const NODE_SHARE_MIN_SCALE: f64 = 0.6;
const NODE_SHARE_MAX_SCALE: f64 = 1.6;
const SCORE_DROP_CAP: i32 = 100;
const SCORE_DROP_DIVISOR: f64 = 200.0;
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Shared between the UCI loop and all search threads of one `go`.
pub struct SearchControl {
    stop: AtomicBool,
    start: Instant,
    /// When the clock started, in ms after `start`: later than 0 for a
    /// search that ran untimed until `ponderhit`.
    clock_start_ms: AtomicU64,
    soft_deadline_ms: AtomicU64,
    hard_deadline_ms: AtomicU64,
    /// Nodes the search's threads have added so far, each in chunks of
    /// `TIME_CHECK_INTERVAL`.
    shared_nodes: AtomicU64,
}

impl SearchControl {
    pub fn new(budget: Option<TimeBudget>) -> Arc<Self> {
        let (soft, hard) = budget.map_or((NO_DEADLINE, NO_DEADLINE), |b| (b.soft_ms, b.hard_ms));
        Arc::new(Self {
            stop: AtomicBool::new(false),
            start: Instant::now(),
            clock_start_ms: AtomicU64::new(0),
            soft_deadline_ms: AtomicU64::new(soft),
            hard_deadline_ms: AtomicU64::new(hard),
            shared_nodes: AtomicU64::new(0),
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
        self.clock_start_ms.store(now, Ordering::Relaxed);
        self.soft_deadline_ms
            .store(now + budget.soft_ms, Ordering::Relaxed);
        // Release: a search that sees the deadline sees the start and soft
        // deadline stored before it.
        self.hard_deadline_ms
            .store(now + budget.hard_ms, Ordering::Release);
    }

    fn is_timed(&self) -> bool {
        self.hard_deadline_ms.load(Ordering::Acquire) != NO_DEADLINE
    }

    /// Milliseconds since the clock started.
    fn clock_ms(&self) -> u64 {
        self.elapsed_ms()
            .saturating_sub(self.clock_start_ms.load(Ordering::Relaxed))
    }

    /// The soft time budget, measured from when the clock started.
    fn soft_budget_ms(&self) -> u64 {
        self.soft_deadline_ms()
            .saturating_sub(self.clock_start_ms.load(Ordering::Relaxed))
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

    fn add_nodes(&self, nodes: u64) {
        self.shared_nodes.fetch_add(nodes, Ordering::Relaxed);
    }

    fn shared_nodes(&self) -> u64 {
        self.shared_nodes.load(Ordering::Relaxed)
    }
}

/// A node budget for the main thread: no new iteration once `soft` nodes are searched,
/// and an immediate stop at `hard`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct NodeLimit {
    pub soft: u64,
    pub hard: u64,
}

#[derive(Clone)]
pub struct SearchLimits {
    pub max_depth: usize,
    pub params: SearchParams,
    /// `go infinite` / `go ponder`: never return before `stop` (or `ponderhit` sets a budget).
    pub wait_for_stop: bool,
    pub nodes: Option<NodeLimit>,
    /// `go searchmoves`: the only root moves to consider, each legal; every
    /// legal move when `None`.
    pub root_moves: Option<MoveList>,
}

pub struct SearchResult {
    pub best_move: Option<Move>,
    pub ponder_move: Option<Move>,
    pub nodes: u64,
    /// Score of the last completed iteration, for the side to move.
    pub score: Option<Score>,
}

/// What one thread's iterative deepening found.
struct ThreadOutcome {
    best_move: Option<Move>,
    nodes: u64,
    /// Search score of the last completed iteration.
    score: Option<i32>,
}

/// Move-ordering statistics one search thread keeps between `go` commands.
pub struct ThreadData {
    histories: Histories,
    correction: CorrectionHistory,
    counter_moves: [[[Option<Move>; 64]; 13]; 2],
}

impl ThreadData {
    fn new() -> Box<Self> {
        Box::new(Self {
            histories: Histories::new(),
            correction: CorrectionHistory::new(),
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

    pub const fn count(&self) -> usize {
        self.data.len()
    }

    /// Forgets everything learned in previous games (`ucinewgame`).
    pub fn clear(&mut self) {
        let count = self.count();
        *self = Self::new(count);
    }
}

/// Late-move reductions by depth and move number, built from the search parameters.
struct LmrTables {
    quiet: [[i32; LMR_TABLE_SIZE]; LMR_TABLE_SIZE],
    capture: [[i32; LMR_TABLE_SIZE]; LMR_TABLE_SIZE],
}

impl LmrTables {
    fn new(params: &SearchParams) -> Self {
        let ratio = |x100: i32| f64::from(x100) / 100.0;
        let (quiet_base, quiet_divisor) = (
            ratio(params.lmr_quiet_base_x100),
            ratio(params.lmr_quiet_divisor_x100),
        );
        let (capture_base, capture_divisor) = (
            ratio(params.lmr_capture_base_x100),
            ratio(params.lmr_capture_divisor_x100),
        );
        let mut tables = Self {
            quiet: [[0; LMR_TABLE_SIZE]; LMR_TABLE_SIZE],
            capture: [[0; LMR_TABLE_SIZE]; LMR_TABLE_SIZE],
        };
        for depth in 1..LMR_TABLE_SIZE {
            for moves in 1..LMR_TABLE_SIZE {
                let product = (depth as f64).ln() * (moves as f64).ln();
                tables.quiet[depth][moves] = (quiet_base + product / quiet_divisor) as i32;
                tables.capture[depth][moves] = (capture_base + product / capture_divisor) as i32;
            }
        }
        tables
    }

    /// Base reduction for the `moves_searched`-th move at `depth`.
    fn base_reduction(&self, depth: i32, moves_searched: i32, is_quiet: bool) -> i32 {
        let d = (depth.max(0) as usize).min(LMR_TABLE_SIZE - 1);
        let m = (moves_searched.max(0) as usize).min(LMR_TABLE_SIZE - 1);
        if is_quiet {
            self.quiet[d][m]
        } else {
            self.capture[d][m]
        }
    }
}

struct TimeManager {
    control: Arc<SearchControl>,
    is_main_thread: bool,
    node_limit: Option<NodeLimit>,
    nodes: u64,
    best_move_stability: usize,
    previous_best_move: Option<Move>,
}

impl TimeManager {
    const fn new(
        control: Arc<SearchControl>,
        is_main_thread: bool,
        node_limit: Option<NodeLimit>,
    ) -> Self {
        Self {
            control,
            is_main_thread,
            node_limit,
            nodes: 0,
            best_move_stability: 0,
            previous_best_move: None,
        }
    }

    /// Counts a node, adding a chunk to every thread's shared count at the
    /// interval the clock is checked.
    #[inline(always)]
    fn count_node(&mut self) {
        self.nodes += 1;
        if self.nodes.is_multiple_of(TIME_CHECK_INTERVAL) {
            self.control.add_nodes(TIME_CHECK_INTERVAL);
        }
    }

    /// Nodes every thread has searched, as far as this one knows: the
    /// shared count with this thread's own nodes since its last chunk.
    fn nodes_of_all_threads(&self) -> u64 {
        self.control.shared_nodes() + self.nodes % TIME_CHECK_INTERVAL
    }

    #[inline(always)]
    fn check_hard_limit(&self) -> bool {
        if self.control.is_stopped() {
            return true;
        }
        if self.is_main_thread
            && let Some(limit) = self.node_limit
            && self.nodes >= limit.hard
        {
            self.control.stop();
            return true;
        }
        if self.is_main_thread
            && self.nodes.is_multiple_of(TIME_CHECK_INTERVAL)
            && self.control.elapsed_ms() >= self.control.hard_deadline_ms()
        {
            self.control.stop();
            return true;
        }
        false
    }

    /// Decides after a completed iteration whether another one is worth starting.
    fn should_stop_iterating(&mut self, depth: usize, iteration: &IterationSummary) -> bool {
        let best_move = iteration.best_move;
        if best_move == self.previous_best_move {
            self.best_move_stability += 1;
        } else {
            self.best_move_stability = 0;
        }
        self.previous_best_move = best_move;

        if self.is_main_thread
            && self
                .node_limit
                .is_some_and(|limit| self.nodes >= limit.soft)
        {
            return true;
        }
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
        let target = self.control.soft_budget_ms() as f64
            * stability_scale
            * node_share_scale(iteration.best_move_node_share)
            * score_drop_scale(iteration.score_drop);
        self.control.clock_ms() as f64 >= target * NEW_ITERATION_FRACTION
            || self.control.elapsed_ms() >= self.control.hard_deadline_ms()
    }
}

/// What iterative deepening learned from the iteration that just finished.
struct IterationSummary {
    best_move: Option<Move>,
    /// Fraction of all root nodes so far that went into the best move's subtree.
    best_move_node_share: f64,
    /// How far the score fell since the previous iteration (0 if it rose).
    score_drop: i32,
}

/// Spend less time when one move absorbs most of the search, more when it is contested.
fn node_share_scale(best_move_node_share: f64) -> f64 {
    (NODE_SHARE_PIVOT - best_move_node_share).clamp(NODE_SHARE_MIN_SCALE, NODE_SHARE_MAX_SCALE)
}

/// Spend more time when the evaluation is falling.
fn score_drop_scale(score_drop: i32) -> f64 {
    1.0 + f64::from(score_drop.clamp(0, SCORE_DROP_CAP)) / SCORE_DROP_DIVISOR
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
    /// Beta cutoffs at this ply since the node two plies up started (counting double
    /// when there was no TT move).
    cutoff_count: u8,
    /// The node is or was on a principal variation (from the TT).
    tt_pv: bool,
}

impl Default for StackEntry {
    fn default() -> Self {
        Self {
            static_eval: None,
            current_move: None,
            moved_piece: Piece::Empty,
            excluded_move: None,
            double_extensions: 0,
            cutoff_count: 0,
            tt_pv: false,
        }
    }
}

struct Search<'a> {
    board: Board,
    tt: &'a SharedTransTable,
    thread_data: &'a mut ThreadData,
    time: TimeManager,
    killers: [[Option<Move>; 2]; MAX_PLY],
    stack: [StackEntry; MAX_PLY + 1],
    ply: usize,
    seldepth: usize,
    /// Best root move found so far: the first one searched, then each that
    /// raised alpha in a finished subtree.
    root_best: Option<(Move, i32)>,
    /// Nodes searched under each root move during this `go`.
    root_move_nodes: Vec<(Move, u64)>,
    /// The root moves `go searchmoves` allows, or `None` for all of them.
    allowed_root_moves: Option<&'a [Move]>,
    params: SearchParams,
    lmr: LmrTables,
}

/// True when the fifty-move rule ends the game at `board`: a hundred
/// half-moves without a capture or pawn move, unless the side to move is
/// checkmated, which the mate decides.
fn is_fifty_move_draw(board: &Board) -> bool {
    board.halfmove_clock >= 100 && (!board.in_check() || board.has_legal_move())
}

/// Makes `m` if it is legal in `pos`.
fn make_if_legal(pos: &mut Board, m: Move) -> Option<Undo> {
    let info = pos.legality_info()?;
    (pos.is_pseudo_legal(m) && pos.is_legal(m, &info)).then(|| pos.make_move(m))
}

/// Piece-index for the counter-move table: the piece that made `prev` now stands on `prev.to`.
#[inline(always)]
const fn counter_move_slot(board: &Board, prev: Move) -> (usize, usize, usize) {
    (
        prev.is_capture() as usize,
        board.piece_on[prev.to.index()].index(),
        prev.to.index(),
    )
}

/// Mate scores are stored relative to the node, not the root.
#[inline(always)]
const fn score_to_tt(score: i32, ply: usize) -> i32 {
    if score > MATE_THRESHOLD {
        score + ply as i32
    } else if score < -MATE_THRESHOLD {
        score - ply as i32
    } else {
        score
    }
}

#[inline(always)]
const fn score_from_tt(score: i32, ply: usize) -> i32 {
    if score > MATE_THRESHOLD {
        score - ply as i32
    } else if score < -MATE_THRESHOLD {
        score + ply as i32
    } else {
        score
    }
}

#[inline(always)]
const fn tt_cutoff_allowed(bound: Bound, score: i32, alpha: i32, beta: i32) -> bool {
    match bound {
        Bound::Exact => true,
        Bound::Lower => score >= beta,
        Bound::Upper => score <= alpha,
    }
}

/// Whether `score`, a search result with `bound`, lies past `eval` on the
/// side the bound looks: anywhere when exact, above for a lower bound,
/// below for an upper one.
#[inline(always)]
const fn bound_beyond(bound: Bound, score: i32, eval: i32) -> bool {
    match bound {
        Bound::Exact => true,
        Bound::Lower => score > eval,
        Bound::Upper => score < eval,
    }
}

/// `eval`, or the TT entry's score where its bound places the position's
/// value past `eval`.
#[inline(always)]
fn refined_eval(entry: Option<TTEntry>, eval: i32, ply: usize) -> i32 {
    match entry {
        Some(entry) if bound_beyond(entry.bound(), score_from_tt(entry.score(), ply), eval) => {
            score_from_tt(entry.score(), ply)
        }
        _ => eval,
    }
}

#[inline(always)]
const fn bound_for(best_score: i32, alpha_orig: i32, beta: i32) -> Bound {
    if best_score <= alpha_orig {
        Bound::Upper
    } else if best_score >= beta {
        Bound::Lower
    } else {
        Bound::Exact
    }
}

const fn has_non_pawn_material(board: &Board) -> bool {
    let our_pieces = board.pieces_of(board.turn);
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

    fn is_allowed_root_move(&self, m: Move) -> bool {
        self.allowed_root_moves
            .is_none_or(|allowed| allowed.contains(&m))
    }

    fn record_root_move_nodes(&mut self, m: Move, nodes: u64) {
        match self
            .root_move_nodes
            .iter_mut()
            .find(|(root_move, _)| *root_move == m)
        {
            Some((_, total)) => *total += nodes,
            None => self.root_move_nodes.push((m, nodes)),
        }
    }

    fn root_move_node_share(&self, m: Move) -> f64 {
        let total: u64 = self.root_move_nodes.iter().map(|&(_, n)| n).sum();
        let best = self
            .root_move_nodes
            .iter()
            .find(|(root_move, _)| *root_move == m)
            .map_or(0, |&(_, n)| n);
        if total == 0 {
            0.0
        } else {
            best as f64 / total as f64
        }
    }

    const fn previous_move(&self) -> Option<Move> {
        if self.ply == 0 {
            None
        } else {
            self.stack[self.ply - 1].current_move
        }
    }

    /// The move that last refuted the previous move, played where it was.
    fn counter_move(&self) -> Option<Move> {
        let (capture, piece, to) = counter_move_slot(&self.board, self.previous_move()?);
        self.thread_data.counter_moves[capture][piece][to]
    }

    /// The static evaluations of a node not in check, given its TT entry.
    fn evaluate_node(&mut self, tt_entry: Option<TTEntry>) -> NodeEval {
        let raw = self.static_eval(tt_entry.and_then(|e| e.eval()));
        let corrected = self.corrected_eval(raw);
        let refined = refined_eval(tt_entry, corrected, self.ply);
        NodeEval {
            raw,
            corrected,
            refined,
        }
    }

    /// Static eval for the current node, reusing the TT's copy when it has one.
    fn static_eval(&mut self, tt_eval: Option<i32>) -> i32 {
        tt_eval.unwrap_or_else(|| cp_to_search_units(evaluate(&mut self.board)))
    }

    /// Static eval adjusted by what searches have learned about this pawn structure.
    fn corrected_eval(&self, raw_eval: i32) -> i32 {
        let correction = self
            .thread_data
            .correction
            .correction(self.board.turn, self.board.pawn_key());
        (raw_eval + correction).clamp(-MATE_THRESHOLD + 1, MATE_THRESHOLD - 1)
    }

    /// Makes `m` (pseudo-legal in the current position) unless it leaves the king in check.
    fn make_move(&mut self, m: Move, legality: &LegalityInfo) -> Option<Undo> {
        if !self.board.is_legal(m, legality) {
            return None;
        }
        let moved_piece = self.board.piece_on[m.from.index()];
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

    /// Continuation-history keys for the moves played one and two plies before `ply`.
    fn continuation_keys_at(&self, ply: usize) -> [Option<ContinuationKey>; 2] {
        let key = |back: usize| {
            let entry = self.stack[ply.checked_sub(back)?];
            entry
                .current_move
                .map(|m| ContinuationKey::new(entry.moved_piece, m.to))
        };
        [key(1), key(2)]
    }

    fn continuation_keys(&self) -> [Option<ContinuationKey>; 2] {
        self.continuation_keys_at(self.ply)
    }

    /// The score that ends a node below the root before it is searched (a
    /// draw, or the evaluation at the ply limit), or else its window narrowed
    /// to the mate scores still reachable, which ends it when empty.
    fn window_below_root(&mut self, alpha: i32, beta: i32) -> ControlFlow<i32, (i32, i32)> {
        if self.board.is_repetition(self.ply) || is_fifty_move_draw(&self.board) {
            return ControlFlow::Break(DRAW_SCORE);
        }
        if self.ply >= MAX_PLY - 1 {
            return ControlFlow::Break(cp_to_search_units(evaluate(&mut self.board)));
        }
        let alpha = alpha.max(-MATE_SCORE + self.ply as i32);
        let beta = beta.min(MATE_SCORE - self.ply as i32 - 1);
        if alpha >= beta {
            return ControlFlow::Break(alpha);
        }
        ControlFlow::Continue((alpha, beta))
    }

    /// Whether `m` is a capture quiescence skips: one that loses material, or
    /// one whose victim, with a margin, cannot lift `stand_pat` to `alpha`.
    /// The victim's SEE value, near centipawns, is added to a score in search
    /// units as it is; `delta_margin` was tuned that way.
    fn is_futile_capture(&self, m: Move, stand_pat: i32, alpha: i32) -> bool {
        if !m.is_capture() {
            return false;
        }
        let victim_falls_short = m.promotion().is_none()
            && self.board.piece_on[m.to.index()]
                .kind()
                .is_some_and(|kind| {
                    stand_pat + PIECE_VALUES[kind as usize] + self.params.delta_margin < alpha
                });
        victim_falls_short || !see_ge(&self.board, m, 0)
    }

    /// The TT's score for this node when `entry` is deep enough and its bound
    /// settles the window, crediting the TT move when that score fails high.
    fn tt_cutoff(&mut self, entry: TTEntry, depth: i32, alpha: i32, beta: i32) -> Option<i32> {
        let tt_score = score_from_tt(entry.score(), self.ply);
        if i32::from(entry.depth()) < depth
            || !tt_cutoff_allowed(entry.bound(), tt_score, alpha, beta)
        {
            return None;
        }
        if tt_score >= beta
            && let Some(m) = entry.best_move()
        {
            self.reward_tt_cutoff_move(m, depth);
        }
        Some(tt_score)
    }

    /// Moves the correction history toward the gap between `static_eval` and
    /// a quiet, non-mate search result that lies past it.
    fn update_correction(&mut self, static_eval: i32, outcome: NodeOutcome) {
        if outcome
            .best_move
            .is_some_and(super::types::Move::is_capture)
            || outcome.best_score.abs() >= MATE_THRESHOLD
            || !bound_beyond(outcome.bound, outcome.best_score, static_eval)
        {
            return;
        }
        let pawn_key = self.board.pawn_key();
        self.thread_data.correction.update(
            self.board.turn,
            pawn_key,
            outcome.best_score - static_eval,
            outcome.depth,
            self.params.correction_max_weight,
        );
    }

    /// Credits a quiet TT move whose stored score still fails high.
    fn reward_tt_cutoff_move(&mut self, m: Move, depth: i32) {
        if !m.is_quiet() || !self.board.is_pseudo_legal(m) {
            return;
        }
        let piece = self.board.piece_on[m.from.index()];
        let continuations = self.continuation_keys();
        self.thread_data.histories.update_quiet(
            self.board.turn,
            m,
            piece,
            &continuations,
            stat_bonus(depth, &self.params),
        );
    }

    /// The opponent's last move refuted everything here, so it was a good quiet move.
    fn reward_prior_quiet_move(&mut self, depth: i32) {
        let Some(parent_ply) = self.ply.checked_sub(1) else {
            return;
        };
        let parent = self.stack[parent_ply];
        let Some(prior) = parent.current_move else {
            return;
        };
        if !prior.is_quiet() {
            return;
        }
        let continuations = self.continuation_keys_at(parent_ply);
        self.thread_data.histories.update_quiet(
            self.board.turn.other(),
            prior,
            parent.moved_piece,
            &continuations,
            stat_bonus(depth, &self.params),
        );
    }

    fn unmake_move(&mut self, m: Move, undo: Undo) {
        self.ply -= 1;
        self.board.unmake_move(m, undo);
    }

    fn update_capture_history(&mut self, m: Move, bonus: i32) {
        let piece = self.board.piece_on[m.from.index()];
        if let Some(victim) = captured_kind(self.board.piece_on[m.to.index()], m) {
            self.thread_data
                .histories
                .update_capture(piece, m.to, victim, bonus);
        }
    }

    /// Rewards the move that caused a beta cutoff and penalizes the moves tried before it.
    fn update_cutoff_stats(&mut self, best: Move, depth: i32, tried: &TriedMoves) {
        let bonus = stat_bonus(depth, &self.params);
        let continuations = self.continuation_keys();
        let color = self.board.turn;

        if best.is_capture() {
            self.update_capture_history(best, bonus);
        } else {
            let ply = self.ply;
            if Some(best) != self.killers[ply][0] {
                self.killers[ply][1] = self.killers[ply][0];
                self.killers[ply][0] = Some(best);
            }
            if let Some(prev) = self.previous_move() {
                let (capture, piece, to) = counter_move_slot(&self.board, prev);
                self.thread_data.counter_moves[capture][piece][to] = Some(best);
            }
            let piece = self.board.piece_on[best.from.index()];
            self.thread_data
                .histories
                .update_quiet(color, best, piece, &continuations, bonus);
            for &quiet in tried.quiets.as_slice() {
                let piece = self.board.piece_on[quiet.from.index()];
                self.thread_data.histories.update_quiet(
                    color,
                    quiet,
                    piece,
                    &continuations,
                    -bonus,
                );
            }
        }

        for &capture in tried.captures.as_slice() {
            self.update_capture_history(capture, -bonus);
        }
    }
}

fn quiesce(search: &mut Search, mut alpha: i32, beta: i32) -> Result<i32, Aborted> {
    use crate::movepicker::QMovePicker;

    search.seldepth = search.seldepth.max(search.ply);
    search.time.count_node();

    if search.time.check_hard_limit() {
        return Err(Aborted);
    }
    if search.ply >= MAX_PLY - 1 {
        return Ok(cp_to_search_units(evaluate(&mut search.board)));
    }

    let is_pv = beta - alpha > 1;
    let alpha_orig = alpha;
    let key = search.board.zobrist;
    let tt_entry = search.tt.probe(key);
    let tt_pv = tt_entry.is_some_and(|e| e.is_pv());
    if let Some(entry) = tt_entry {
        let tt_score = score_from_tt(entry.score(), search.ply);
        if !is_pv && tt_cutoff_allowed(entry.bound(), tt_score, alpha, beta) {
            return Ok(tt_score);
        }
    }

    let Some(legality) = search.board.legality_info() else {
        // Unreachable from a parsed FEN: every position has both kings.
        return Ok(DRAW_SCORE);
    };
    if legality.in_check() {
        return quiesce_evasions(search, alpha, beta, &legality);
    }

    let node_eval = search.evaluate_node(tt_entry);
    let stand_pat = node_eval.refined;
    if stand_pat >= beta {
        if tt_entry.is_none() {
            search.tt.store(
                key,
                TTWrite {
                    depth: 0,
                    score: score_to_tt(stand_pat, search.ply),
                    eval: Some(node_eval.raw),
                    bound: Bound::Lower,
                    best_move: None,
                    is_pv: tt_pv,
                },
            );
        }
        return Ok(stand_pat);
    }
    alpha = alpha.max(stand_pat);

    let mut best_score = stand_pat;
    let mut best_move = None;
    let mut picker = QMovePicker::new();
    while let Some(m) = picker.next(&search.board) {
        if search.is_futile_capture(m, stand_pat, alpha) {
            continue;
        }
        let Some(undo) = search.make_move(m, &legality) else {
            continue;
        };
        let score = quiesce(search, -beta, -alpha);
        search.unmake_move(m, undo);
        let score = -score?;

        if search.time.check_hard_limit() {
            return Err(Aborted);
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
    search.tt.store(
        key,
        TTWrite {
            depth: 0,
            score: score_to_tt(best_score, search.ply),
            eval: Some(node_eval.raw),
            bound,
            best_move,
            is_pv: tt_pv,
        },
    );
    Ok(best_score)
}

/// Quiescence when in check: every evasion is searched so mates are found.
fn quiesce_evasions(
    search: &mut Search,
    mut alpha: i32,
    beta: i32,
    legality: &LegalityInfo,
) -> Result<i32, Aborted> {
    let mut evasions = ScoredMoves::new();
    evasions.fill(
        |moves| search.board.generate_pseudo_legal_moves(moves),
        |m| {
            if m.is_capture() {
                see(&search.board, m)
            } else {
                0
            }
        },
    );

    let mut best_score = -MATE_SCORE + search.ply as i32;
    while let Some(m) = evasions.pick_best() {
        let Some(undo) = search.make_move(m, legality) else {
            continue;
        };
        let score = quiesce(search, -beta, -alpha);
        search.unmake_move(m, undo);
        let score = -score?;

        if search.time.check_hard_limit() {
            return Err(Aborted);
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
    Ok(best_score)
}

/// What razoring, reverse futility and null-move pruning weigh about a
/// non-PV node with a static evaluation.
#[derive(Clone, Copy)]
struct NodePruning {
    depth: i32,
    eval: i32,
    alpha: i32,
    beta: i32,
    improving: bool,
    node: NodeKind,
    /// The node is searched without its TT move to test that move's singularity.
    is_singular_search: bool,
}

/// The score a node returns without searching its moves, when its static
/// evaluation is far enough outside the window.
fn prune_node(search: &mut Search, n: NodePruning) -> Result<Option<i32>, Aborted> {
    let razor_margin =
        search.params.razor_margin + search.params.razor_depth_sq_margin * n.depth * n.depth;
    if n.eval < n.alpha - razor_margin {
        let q_score = quiesce(search, n.alpha, n.beta)?;
        if q_score < n.alpha {
            return Ok(Some(q_score));
        }
    }
    if n.depth <= search.params.rfp_max_depth
        && n.eval - search.params.rfp_margin * (n.depth - i32::from(n.improving)) >= n.beta
    {
        return Ok(Some(n.eval));
    }
    null_move_cutoff(search, n)
}

/// Passes the move and searches the reply shallower; a side that still fails
/// high after giving its opponent a free move fails high here too.
fn null_move_cutoff(search: &mut Search, n: NodePruning) -> Result<Option<i32>, Aborted> {
    let previous_was_null = search.ply > 0 && search.stack[search.ply - 1].current_move.is_none();
    if n.depth < search.params.nmp_min_depth
        || n.is_singular_search
        || n.eval < n.beta
        || previous_was_null
        || !has_non_pawn_material(&search.board)
    {
        return Ok(None);
    }
    let r = search.params.nmp_base_reduction
        + n.depth / search.params.nmp_depth_divisor
        + ((n.eval - n.beta) / search.params.nmp_eval_divisor).min(search.params.nmp_eval_max);
    let undo = search.make_null_move();
    let null_score = negamax(search, -n.beta, -n.beta + 1, n.depth - r, n.node.opposite());
    search.unmake_null_move(undo);
    let null_score = -null_score?;
    if search.time.check_hard_limit() {
        return Err(Aborted);
    }
    if null_score < n.beta {
        return Ok(None);
    }
    // A mate found after passing proves nothing about the real moves.
    Ok(Some(if null_score > MATE_THRESHOLD {
        n.beta
    } else {
        null_score
    }))
}

/// What `ProbCut` weighs about a non-PV node not in check.
#[derive(Clone, Copy)]
struct ProbCutNode {
    depth: i32,
    beta: i32,
    static_eval: i32,
    node: NodeKind,
    tt_entry: Option<TTEntry>,
}

/// Looks for a capture that beats beta by a margin in quiescence and then in a
/// reduced search; if one exists the node almost certainly fails high at full
/// depth too. Skipped near mate scores and where the TT entry already falls short.
fn probcut(
    search: &mut Search,
    legality: &LegalityInfo,
    n: ProbCutNode,
) -> Result<Option<i32>, Aborted> {
    use crate::movepicker::QMovePicker;

    if n.depth < search.params.probcut_min_depth || n.beta.abs() >= MATE_THRESHOLD {
        return Ok(None);
    }
    let probcut_beta = n.beta + search.params.probcut_margin;
    let tt_rules_out_probcut = n.tt_entry.is_some_and(|entry| {
        i32::from(entry.depth()) >= n.depth - 3
            && score_from_tt(entry.score(), search.ply) < probcut_beta
    });
    if tt_rules_out_probcut {
        return Ok(None);
    }
    let probe_depth = n.depth - search.params.probcut_reduction;
    let mut picker = QMovePicker::new();
    while let Some(m) = picker.next(&search.board) {
        if !see_ge(&search.board, m, probcut_beta - n.static_eval) {
            continue;
        }
        let Some(undo) = search.make_move(m, legality) else {
            continue;
        };
        let score = probe_capture(search, probcut_beta, probe_depth, n.node);
        search.unmake_move(m, undo);
        let score = score?;

        if search.time.check_hard_limit() {
            return Err(Aborted);
        }
        if score >= probcut_beta {
            search.tt.store(
                search.board.zobrist,
                TTWrite {
                    depth: (probe_depth + 1) as i16,
                    score: score_to_tt(score, search.ply),
                    eval: None,
                    bound: Bound::Lower,
                    best_move: Some(m),
                    is_pv: search.stack[search.ply].tt_pv,
                },
            );
            return Ok(Some(score));
        }
    }
    Ok(None)
}

/// The score of a `ProbCut` capture already made: its quiescence score, and if
/// that beats `probcut_beta`, a reduced search's.
fn probe_capture(
    search: &mut Search,
    probcut_beta: i32,
    probe_depth: i32,
    node: NodeKind,
) -> Result<i32, Aborted> {
    let q_score = -quiesce(search, -probcut_beta, -probcut_beta + 1)?;
    if q_score < probcut_beta {
        return Ok(q_score);
    }
    Ok(-negamax(
        search,
        -probcut_beta,
        -probcut_beta + 1,
        probe_depth,
        node.opposite(),
    )?)
}

enum Singularity {
    /// Search the move this many plies deeper (negative: shallower).
    Extend(i32),
    /// Even without the TT move the node fails high; return this score.
    MultiCut(i32),
}

/// Verifies whether the TT move is singular by searching the node without it.
fn singular_extension(
    search: &mut Search,
    tt_move: Move,
    tt_entry: Option<crate::tt::TTEntry>,
    depth: i32,
    beta: i32,
    is_pv: bool,
    node: NodeKind,
) -> Result<Singularity, Aborted> {
    let Some(entry) = tt_entry else {
        return Ok(Singularity::Extend(0));
    };
    let tt_score = score_from_tt(entry.score(), search.ply);
    let reliable = depth >= search.params.singular_min_depth
        && i32::from(entry.depth()) >= depth - search.params.singular_tt_depth_margin
        && entry.bound() != Bound::Upper
        && tt_score.abs() < MATE_THRESHOLD;
    if !reliable {
        return Ok(Singularity::Extend(0));
    }

    let singular_beta = tt_score - search.params.singular_beta_per_depth_x16 * depth / 16;
    let singular_depth = (depth - 1) / 2;
    search.stack[search.ply].excluded_move = Some(tt_move);
    let singular_score = negamax(
        search,
        singular_beta - 1,
        singular_beta,
        singular_depth,
        node,
    );
    search.stack[search.ply].excluded_move = None;
    let singular_score = singular_score?;
    if search.time.check_hard_limit() {
        return Err(Aborted);
    }

    Ok(if singular_score < singular_beta {
        let double = !is_pv
            && singular_score < singular_beta - search.params.double_extension_margin
            && search.stack[search.ply].double_extensions < MAX_DOUBLE_EXTENSIONS;
        Singularity::Extend(if double { 2 } else { 1 })
    } else if singular_beta >= beta {
        Singularity::MultiCut(singular_beta)
    } else if tt_score >= beta {
        Singularity::Extend(-1)
    } else {
        Singularity::Extend(0)
    })
}

/// What the move loop does with a move before searching it.
enum Pruning {
    Search,
    Skip,
    /// Skip it and every quiet move after it.
    SkipQuiets,
}

/// What the move-pruning rules weigh about a move and its node.
#[derive(Clone, Copy)]
struct PruningCandidate {
    depth: i32,
    eval: i32,
    alpha: i32,
    is_quiet: bool,
    quiets_seen: i32,
    lmp_limit: i32,
    quiet_history: i32,
}

impl Search<'_> {
    /// Applies late-move, history, futility and SEE pruning to `m`.
    fn prune_move(&self, m: Move, c: PruningCandidate) -> Pruning {
        let params = &self.params;
        let shallow = c.depth <= params.move_pruning_max_depth;
        if !c.is_quiet {
            let losing = shallow && !see_ge(&self.board, m, -params.see_capture_margin * c.depth);
            return if losing {
                Pruning::Skip
            } else {
                Pruning::Search
            };
        }
        if c.quiets_seen > c.lmp_limit {
            return Pruning::SkipQuiets;
        }
        if c.depth <= params.history_pruning_max_depth
            && c.quiet_history < -params.history_pruning_margin * c.depth
        {
            return Pruning::Skip;
        }
        if shallow && c.eval + params.futility_base + params.futility_per_depth * c.depth <= c.alpha
        {
            return Pruning::SkipQuiets;
        }
        if shallow && !see_ge(&self.board, m, -params.see_quiet_margin * c.depth * c.depth) {
            return Pruning::Skip;
        }
        Pruning::Search
    }
}

/// The static evaluations of a node not in check.
#[derive(Clone, Copy)]
struct NodeEval {
    /// The evaluation itself, which the TT keeps.
    raw: i32,
    /// `raw` adjusted by what searches learned about the pawn structure.
    corrected: i32,
    /// `corrected`, or the TT score where its bound places the value past it.
    refined: i32,
}

/// The first quiet moves and captures a node searched without a cutoff, whose
/// histories a later cutoff lowers.
#[derive(Default)]
struct TriedMoves {
    quiets: StackVec<Move, MAX_TRIED_MOVES>,
    captures: StackVec<Move, MAX_TRIED_MOVES>,
}

impl TriedMoves {
    const fn record(&mut self, m: Move) {
        let list = if m.is_quiet() {
            &mut self.quiets
        } else if m.is_capture() {
            &mut self.captures
        } else {
            return;
        };
        if list.len() < MAX_TRIED_MOVES {
            list.push(m);
        }
    }
}
/// What searching a node's moves found.
#[derive(Clone, Copy)]
struct NodeOutcome {
    depth: i32,
    best_score: i32,
    best_move: Option<Move>,
    bound: Bound,
}

/// A node's window and the best score its moves have reached so far.
#[derive(Clone, Copy)]
struct NodeBounds {
    alpha: i32,
    beta: i32,
    best_score: i32,
}

/// Searches a move after the first, already made on the board: with a null
/// window at reduced depth, again at full depth if it beats alpha, and with
/// the full window if it then lands inside a PV node's window.
fn search_late_move(
    search: &mut Search,
    node_ply: usize,
    m: LateMove,
    bounds: NodeBounds,
) -> Result<i32, Aborted> {
    let NodeBounds {
        alpha,
        beta,
        best_score,
    } = bounds;
    let reduction = search.late_move_reduction(node_ply, m);
    let reduced_depth = m.new_depth - reduction;
    let mut score = -negamax(search, -alpha - 1, -alpha, reduced_depth, NodeKind::Cut)?;
    if score > alpha && reduction > 0 {
        let deeper = score > best_score + search.params.deeper_margin + 2 * m.new_depth;
        let shallower = score < best_score + search.params.shallower_margin;
        let research_depth = m.new_depth + i32::from(deeper) - i32::from(shallower);
        if research_depth > reduced_depth {
            score = -negamax(
                search,
                -alpha - 1,
                -alpha,
                research_depth,
                m.node.opposite(),
            )?;
        }
    }
    if m.is_pv && score > alpha && score < beta {
        score = -negamax(search, -beta, -alpha, m.new_depth, NodeKind::All)?;
    }
    Ok(score)
}

/// What a late move's reduction weighs about the move and its node.
#[derive(Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about the move"
)]
struct LateMove {
    depth: i32,
    /// The depth the move is searched to before any reduction.
    new_depth: i32,
    moves_searched: i32,
    quiet_history: i32,
    is_quiet: bool,
    is_pv: bool,
    in_check: bool,
    node: NodeKind,
    improving: bool,
    killer_or_counter: bool,
    tt_capture: bool,
    tt_pv: bool,
}

impl Search<'_> {
    /// How many plies to reduce a late move by, with the move made on the
    /// board and `node_ply` the ply of the node it was played from.
    fn late_move_reduction(&self, node_ply: usize, m: LateMove) -> i32 {
        let params = &self.params;
        if m.depth < params.lmr_min_depth || m.in_check || (m.is_pv && !m.is_quiet) {
            return 0;
        }
        let gives_check = self.board.in_check();
        let mut reduction_1024 = 1024
            * self
                .lmr
                .base_reduction(m.depth, m.moves_searched, m.is_quiet);
        reduction_1024 += if m.is_pv { 0 } else { params.lmr_non_pv_x1024 };
        reduction_1024 += if m.node == NodeKind::Cut {
            params.lmr_cut_node_x1024
        } else {
            0
        };
        reduction_1024 += if m.improving {
            0
        } else {
            params.lmr_not_improving_x1024
        };
        reduction_1024 -= if m.killer_or_counter {
            params.lmr_killer_or_counter_x1024
        } else {
            0
        };
        reduction_1024 -= if gives_check {
            params.lmr_gives_check_x1024
        } else {
            0
        };
        reduction_1024 += if m.tt_capture {
            params.lmr_tt_capture_x1024
        } else {
            0
        };
        reduction_1024 -= if m.tt_pv { params.lmr_tt_pv_x1024 } else { 0 };
        let child_cutoffs = i32::from(self.stack[node_ply + 1].cutoff_count);
        reduction_1024 += if child_cutoffs > params.child_cutoff_threshold {
            params.lmr_child_cutoffs_x1024
        } else {
            0
        };
        let r = reduction_1024 / 1024 - m.quiet_history / params.lmr_history_divisor;
        r.clamp(0, (m.new_depth - 1).max(0))
    }
}

/// A search stopped by the clock, a node limit or `stop` before it finished;
/// whatever it had found is not a score.
struct Aborted;

/// What a node is expected to do, as Stockfish's cut and all nodes; a PV
/// node, told apart by its open window, counts as an all node.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NodeKind {
    /// Expected to fail high.
    Cut,
    All,
}

impl NodeKind {
    /// The kind a child that answers this node is expected to be.
    const fn opposite(self) -> Self {
        match self {
            Self::Cut => Self::All,
            Self::All => Self::Cut,
        }
    }

    /// The kind the first move leads to: a cut node below a non-PV all
    /// node, an all node otherwise.
    const fn first_child(self, is_pv: bool) -> Self {
        if !is_pv && matches!(self, Self::All) {
            Self::Cut
        } else {
            Self::All
        }
    }
}

fn negamax(
    search: &mut Search,
    alpha: i32,
    beta: i32,
    depth: i32,
    kind: NodeKind,
) -> Result<i32, Aborted> {
    let node = match enter_node(search, alpha, beta, depth, kind)? {
        ControlFlow::Break(score) => return Ok(score),
        ControlFlow::Continue(node) => node,
    };
    match search_moves(search, &node)? {
        ControlFlow::Break(score) => Ok(score),
        ControlFlow::Continue(searched) => Ok(finish_node(search, &node, &searched)),
    }
}

/// What a node knows once nothing before its move loop has ended it.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about the node"
)]
struct Node {
    kind: NodeKind,
    depth: i32,
    /// The window's lower bound before any move raised it.
    alpha: i32,
    beta: i32,
    is_root: bool,
    is_pv: bool,
    in_check: bool,
    /// The move a singular-extension search leaves out.
    excluded_move: Option<Move>,
    key: ZKey,
    tt_entry: Option<TTEntry>,
    tt_move: Option<Move>,
    tt_pv: bool,
    /// `None` in check.
    eval: Option<NodeEval>,
    improving: bool,
    legality: LegalityInfo,
}

/// Everything a node does before its moves: the abort and draw checks, the
/// mate-distance window, the TT cutoff, quiescence at the horizon, the
/// static evaluation and the pruning it allows. Ends the node with a score,
/// or hands on what the move loop needs.
fn enter_node(
    search: &mut Search,
    mut alpha: i32,
    mut beta: i32,
    mut depth: i32,
    kind: NodeKind,
) -> Result<ControlFlow<i32, Node>, Aborted> {
    search.seldepth = search.seldepth.max(search.ply);
    if search.time.check_hard_limit() {
        return Err(Aborted);
    }

    let is_root = search.ply == 0;
    if !is_root {
        match search.window_below_root(alpha, beta) {
            ControlFlow::Break(score) => return Ok(ControlFlow::Break(score)),
            ControlFlow::Continue(window) => (alpha, beta) = window,
        }
    }

    let is_pv = beta - alpha > 1;
    let key = search.board.zobrist;
    let excluded_move = search.stack[search.ply].excluded_move;
    if excluded_move.is_none() {
        search.stack[search.ply].double_extensions = if is_root {
            0
        } else {
            search.stack[search.ply - 1].double_extensions
        };
    }

    let tt_entry = search.tt.probe(key);
    let tt_move = tt_entry.and_then(|e| e.best_move());
    if excluded_move.is_none() {
        search.stack[search.ply].tt_pv = is_pv || tt_entry.is_some_and(|e| e.is_pv());
    }
    if !is_pv
        && excluded_move.is_none()
        && let Some(entry) = tt_entry
        && let Some(score) = search.tt_cutoff(entry, depth, alpha, beta)
    {
        return Ok(ControlFlow::Break(score));
    }

    let Some(legality) = search.board.legality_info() else {
        // Unreachable from a parsed FEN: every position has both kings.
        return Ok(ControlFlow::Break(DRAW_SCORE));
    };
    let in_check = legality.in_check();
    if in_check {
        depth += 1;
    }
    if depth <= 0 {
        return quiesce(search, alpha, beta).map(ControlFlow::Break);
    }

    search.time.count_node();
    search.killers[(search.ply + 1).min(MAX_PLY - 1)] = [None; 2];
    search.stack[(search.ply + 2).min(MAX_PLY)].cutoff_count = 0;

    let eval = (!in_check).then(|| search.evaluate_node(tt_entry));
    let corrected_eval = eval.map(|e| e.corrected);
    search.stack[search.ply].static_eval = corrected_eval;
    let improving = search.is_improving(corrected_eval);

    if !is_pv && let Some(e) = eval {
        let candidate = NodePruning {
            depth,
            eval: e.refined,
            alpha,
            beta,
            improving,
            node: kind,
            is_singular_search: excluded_move.is_some(),
        };
        if let Some(score) = prune_node(search, candidate)? {
            return Ok(ControlFlow::Break(score));
        }
    }

    if !is_pv
        && excluded_move.is_none()
        && let Some(static_eval) = corrected_eval
    {
        let candidate = ProbCutNode {
            depth,
            beta,
            static_eval,
            node: kind,
            tt_entry,
        };
        if let Some(score) = probcut(search, &legality, candidate)? {
            return Ok(ControlFlow::Break(score));
        }
    }

    if depth >= search.params.iir_min_depth && tt_move.is_none() && excluded_move.is_none() {
        depth -= 1;
    }

    Ok(ControlFlow::Continue(Node {
        kind,
        depth,
        alpha,
        beta,
        is_root,
        is_pv,
        in_check,
        excluded_move,
        key,
        tt_entry,
        tt_move,
        tt_pv: search.stack[search.ply].tt_pv,
        eval,
        improving,
        legality,
    }))
}

/// The move loop's running state.
struct MoveLoop {
    alpha: i32,
    best_score: i32,
    best_move: Option<Move>,
    moves_searched: i32,
    quiets_seen: i32,
    tried: TriedMoves,
}

impl MoveLoop {
    /// Takes a searched move's `score`; true when it fails high, after the
    /// cutoff has been credited.
    fn take_score(&mut self, search: &mut Search, node: &Node, m: Move, score: i32) -> bool {
        if score <= self.best_score {
            return false;
        }
        self.best_score = score;
        if score <= self.alpha {
            return false;
        }
        self.best_move = Some(m);
        self.alpha = score;
        if self.alpha < node.beta {
            return false;
        }
        search.update_cutoff_stats(m, node.depth, &self.tried);
        let entry = &mut search.stack[search.ply];
        entry.cutoff_count = entry
            .cutoff_count
            .saturating_add(1 + u8::from(node.tt_move.is_none()));
        true
    }
}

/// A move the loop is about to search, and what its search weighs.
#[derive(Clone, Copy)]
struct Candidate {
    m: Move,
    is_quiet: bool,
    quiet_history: i32,
    killer_or_counter: bool,
}

/// What searching one move came to.
enum MoveSearch {
    /// It leaves the king in check.
    Illegal,
    Scored(i32),
    /// The singularity search showed the node fails high without the move.
    MultiCut(i32),
}

/// Searches the node's moves in the picker's order, pruning those the node
/// allows, until one fails high or none is left. Ends the node at once on
/// a multi-cut.
fn search_moves(search: &mut Search, node: &Node) -> Result<ControlFlow<i32, MoveLoop>, Aborted> {
    let killers = search.killers[search.ply];
    let counter_move = search.counter_move();
    let mut picker = MovePicker::new(node.tt_move, killers[0], killers[1], counter_move);
    let continuations = search.continuation_keys();
    let lmp_limit =
        (search.params.lmp_base + node.depth * node.depth) / (2 - i32::from(node.improving));
    let mut state = MoveLoop {
        alpha: node.alpha,
        best_score: -SCORE_BOUND,
        best_move: None,
        moves_searched: 0,
        quiets_seen: 0,
        tried: TriedMoves::default(),
    };

    loop {
        let ordering = OrderingContext {
            histories: &search.thread_data.histories,
            continuations,
        };
        let Some(m) = picker.next(&search.board, &ordering) else {
            break;
        };
        if Some(m) == node.excluded_move || (node.is_root && !search.is_allowed_root_move(m)) {
            continue;
        }
        let is_quiet = m.is_quiet();
        if is_quiet {
            state.quiets_seen += 1;
        }
        let quiet_history = if is_quiet {
            let piece = search.board.piece_on[m.from.index()];
            search
                .thread_data
                .histories
                .quiet_history(search.board.turn, piece, m, &continuations)
        } else {
            0
        };

        let can_prune = !node.is_root
            && !node.in_check
            && state.moves_searched > 0
            && state.best_score > -MATE_THRESHOLD;
        if can_prune && let Some(e) = node.eval {
            let candidate = PruningCandidate {
                depth: node.depth,
                eval: e.refined,
                alpha: state.alpha,
                is_quiet,
                quiets_seen: state.quiets_seen,
                lmp_limit,
                quiet_history,
            };
            match search.prune_move(m, candidate) {
                Pruning::Search => {}
                Pruning::Skip => continue,
                Pruning::SkipQuiets => {
                    picker.skip_quiets();
                    continue;
                }
            }
        }

        let candidate = Candidate {
            m,
            is_quiet,
            quiet_history,
            killer_or_counter: killers.contains(&Some(m)) || Some(m) == counter_move,
        };
        let score = match search_move(search, node, &mut state, candidate)? {
            MoveSearch::Illegal => continue,
            MoveSearch::MultiCut(score) => return Ok(ControlFlow::Break(score)),
            MoveSearch::Scored(score) => score,
        };
        if node.is_root && (search.root_best.is_none() || score > state.alpha) {
            search.root_best = Some((m, score));
        }
        if state.take_score(search, node, m, score) {
            break;
        }
        state.tried.record(m);
    }
    Ok(ControlFlow::Continue(state))
}

/// Searches one move: extended if singular, the first with the full window
/// and later ones through [`search_late_move`].
fn search_move(
    search: &mut Search,
    node: &Node,
    state: &mut MoveLoop,
    c: Candidate,
) -> Result<MoveSearch, Aborted> {
    let extension = if Some(c.m) == node.tt_move && !node.is_root && node.excluded_move.is_none() {
        match singular_extension(
            search,
            c.m,
            node.tt_entry,
            node.depth,
            node.beta,
            node.is_pv,
            node.kind,
        )? {
            Singularity::Extend(plies) => plies,
            Singularity::MultiCut(score) => return Ok(MoveSearch::MultiCut(score)),
        }
    } else {
        0
    };
    let node_ply = search.ply;
    let nodes_before = search.time.nodes;
    let Some(undo) = search.make_move(c.m, &node.legality) else {
        return Ok(MoveSearch::Illegal);
    };
    state.moves_searched += 1;
    let new_depth = node.depth - 1 + extension;
    if extension >= 2 {
        search.stack[node_ply].double_extensions += 1;
    }

    let score = if state.moves_searched == 1 {
        negamax(
            search,
            -node.beta,
            -state.alpha,
            new_depth,
            node.kind.first_child(node.is_pv),
        )
        .map(|score| -score)
    } else {
        let late_move = LateMove {
            depth: node.depth,
            new_depth,
            moves_searched: state.moves_searched,
            quiet_history: c.quiet_history,
            is_quiet: c.is_quiet,
            is_pv: node.is_pv,
            in_check: node.in_check,
            node: node.kind,
            improving: node.improving,
            killer_or_counter: c.killer_or_counter,
            tt_capture: node.tt_move.is_some_and(Move::is_capture),
            tt_pv: node.tt_pv,
        };
        let bounds = NodeBounds {
            alpha: state.alpha,
            beta: node.beta,
            best_score: state.best_score,
        };
        search_late_move(search, node_ply, late_move, bounds)
    };

    search.unmake_move(c.m, undo);
    if extension >= 2 {
        search.stack[node_ply].double_extensions -= 1;
    }
    if node.is_root {
        search.record_root_move_nodes(c.m, search.time.nodes - nodes_before);
    }
    let score = score?;
    if search.time.check_hard_limit() {
        return Err(Aborted);
    }
    Ok(MoveSearch::Scored(score))
}

/// The node's score once its moves are searched, which it records: mate or
/// stalemate when it had no legal move, else the best score, credited to the
/// histories and stored in the TT.
fn finish_node(search: &mut Search, node: &Node, searched: &MoveLoop) -> i32 {
    if searched.moves_searched == 0 {
        return if node.excluded_move.is_some() {
            searched.alpha
        } else if node.in_check {
            -MATE_SCORE + search.ply as i32
        } else {
            DRAW_SCORE
        };
    }
    if node.excluded_move.is_some() {
        return searched.best_score;
    }

    let bound = bound_for(searched.best_score, node.alpha, node.beta);
    if bound == Bound::Upper && !node.is_root {
        search.reward_prior_quiet_move(node.depth);
    }
    if let Some(e) = node.eval {
        let outcome = NodeOutcome {
            depth: node.depth,
            best_score: searched.best_score,
            best_move: searched.best_move,
            bound,
        };
        search.update_correction(e.corrected, outcome);
    }
    search.tt.store(
        node.key,
        TTWrite {
            depth: node.depth as i16,
            score: score_to_tt(searched.best_score, search.ply),
            eval: node.eval.map(|e| e.raw),
            bound,
            best_move: searched.best_move,
            is_pv: node.tt_pv,
        },
    );
    searched.best_score
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

/// A search score as reported to the user: centipawns, or moves until mate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Score {
    Centipawns(i32),
    /// Positive when the side to move mates, negative when it is mated.
    MateIn(i32),
}

impl Score {
    const fn from_search(score: i32) -> Self {
        if score.abs() > MATE_THRESHOLD {
            let mate_in = (MATE_SCORE - score.abs() + 1) / 2;
            Self::MateIn(if score > 0 { mate_in } else { -mate_in })
        } else {
            Self::Centipawns(search_units_to_cp(score))
        }
    }
}

/// What the main thread reports after each completed iteration.
#[derive(Clone, Debug)]
pub struct IterationInfo {
    pub depth: usize,
    pub seldepth: usize,
    /// From the point of view of the side to move at the root.
    pub score: Score,
    pub hashfull_permill: u32,
    /// Nodes searched so far by every thread.
    pub nodes: u64,
    pub nps: u64,
    pub time_ms: u64,
    pub pv: Vec<Move>,
}

/// Receives each completed iteration from the main search thread.
pub type InfoReporter<'a> = &'a (dyn Fn(&IterationInfo) + Sync);

/// Prints an iteration as a UCI `info` line.
pub fn print_uci_info(info: &IterationInfo) {
    let score = match info.score {
        Score::Centipawns(cp) => format!("cp {cp}"),
        Score::MateIn(moves) => format!("mate {moves}"),
    };
    let pv = info
        .pv
        .iter()
        .map(|&m| format_uci(m))
        .collect::<Vec<_>>()
        .join(" ");
    println!(
        "info depth {} seldepth {} score {} hashfull {} nodes {} nps {} time {} pv {}",
        info.depth,
        info.seldepth,
        score,
        info.hashfull_permill,
        info.nodes,
        info.nps,
        info.time_ms,
        pv
    );
}

fn iteration_info(search: &Search, depth: usize, best_move: Move, score: i32) -> IterationInfo {
    let elapsed_ms = search.time.control.elapsed_ms();
    let nodes = search.time.nodes_of_all_threads();
    let nps = (nodes * 1000).checked_div(elapsed_ms).unwrap_or(0);
    let mut after_best = search.board.clone();
    after_best.make_move(best_move);
    let pv = std::iter::once(best_move)
        .chain(get_pv_from_tt(
            after_best,
            search.tt,
            depth.saturating_sub(1),
        ))
        .collect();
    IterationInfo {
        depth,
        seldepth: search.seldepth,
        score: Score::from_search(score),
        hashfull_permill: search.tt.hashfull_permill(),
        nodes,
        nps,
        time_ms: elapsed_ms,
        pv,
    }
}

fn legal_moves(board: &Board) -> MoveList {
    let mut moves = MoveList::new();
    board.generate_legal_moves(&mut moves);
    moves
}

/// What a search thread does besides searching.
#[derive(Clone, Copy)]
enum ThreadRole<'a> {
    /// Keeps the clock and the node limit, and reports each finished iteration.
    Main(InfoReporter<'a>),
    Helper,
}

/// Iterative deepening on one thread.
fn iterative_deepening(
    board: &Board,
    tt: &SharedTransTable,
    thread_data: &mut ThreadData,
    limits: &SearchLimits,
    control: Arc<SearchControl>,
    role: ThreadRole,
) -> ThreadOutcome {
    let is_main_thread = matches!(role, ThreadRole::Main(_));
    let params = limits.params;
    let mut search = Search {
        board: board.clone(),
        tt,
        thread_data,
        time: TimeManager::new(control, is_main_thread, limits.nodes),
        killers: [[None; 2]; MAX_PLY],
        stack: [StackEntry::default(); MAX_PLY + 1],
        ply: 0,
        seldepth: 0,
        root_best: None,
        root_move_nodes: Vec::new(),
        allowed_root_moves: limits.root_moves.as_ref().map(MoveList::as_slice),
        params,
        lmr: LmrTables::new(&params),
    };

    let mut score = 0;
    let mut completed_score = None;
    for depth in 1..=limits.max_depth {
        let previous_score = score;
        search.seldepth = 0;
        let mut delta = search.params.aspiration_delta;
        let (mut alpha, mut beta) = if depth >= ASPIRATION_MIN_DEPTH {
            (
                (score - delta).max(-SCORE_BOUND),
                (score + delta).min(SCORE_BOUND),
            )
        } else {
            (-SCORE_BOUND, SCORE_BOUND)
        };

        let mut aborted = false;
        let mut fail_highs = 0;
        loop {
            let search_depth = (depth as i32 - fail_highs).max(1);
            let iteration = negamax(&mut search, alpha, beta, search_depth, NodeKind::All);
            let (Ok(iteration_score), false) = (iteration, search.time.check_hard_limit()) else {
                aborted = true;
                break;
            };
            score = iteration_score;
            if score <= alpha {
                beta = i32::midpoint(alpha, beta);
                alpha = (score - delta).max(-SCORE_BOUND);
                fail_highs = 0;
            } else if score >= beta {
                beta = (score + delta).min(SCORE_BOUND);
                fail_highs += 1;
            } else {
                break;
            }
            delta += delta / 2;
        }

        if aborted {
            break;
        }
        completed_score = Some(score);

        let best_move = search.root_best.map(|(m, _)| m);
        if let (ThreadRole::Main(report), Some(m)) = (role, best_move) {
            report(&iteration_info(&search, depth, m, score));
        }
        let summary = IterationSummary {
            best_move,
            best_move_node_share: best_move.map_or(0.0, |m| search.root_move_node_share(m)),
            score_drop: if depth > 1 { previous_score - score } else { 0 },
        };
        if search.time.should_stop_iterating(depth, &summary) {
            break;
        }
        if score.abs() > MATE_THRESHOLD {
            break;
        }
    }

    ThreadOutcome {
        best_move: search.root_best.map(|(m, _)| m),
        nodes: search.time.nodes,
        score: completed_score,
    }
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

/// Runs a Lazy-SMP search with one thread per entry of `threads`; thread 0 prints UCI
/// `info` lines and manages time.
pub fn search(
    board: &Board,
    tt: &SharedTransTable,
    limits: &SearchLimits,
    threads: &mut SearchThreads,
    control: &Arc<SearchControl>,
) -> SearchResult {
    search_reporting(board, tt, limits, threads, control, &print_uci_info)
}

/// Like [`search`], but hands each completed iteration to `reporter` instead of printing it.
pub fn search_reporting(
    board: &Board,
    tt: &SharedTransTable,
    limits: &SearchLimits,
    threads: &mut SearchThreads,
    control: &Arc<SearchControl>,
    reporter: InfoReporter,
) -> SearchResult {
    tt.tick_age();

    let legal;
    let root_moves = if let Some(listed) = &limits.root_moves {
        listed.as_slice()
    } else {
        legal = legal_moves(board);
        legal.as_slice()
    };
    if root_moves.is_empty() {
        if limits.wait_for_stop {
            wait_until_released(control);
        }
        return SearchResult {
            best_move: None,
            ponder_move: None,
            nodes: 0,
            score: None,
        };
    }
    if root_moves.len() == 1 && control.is_timed() && !limits.wait_for_stop {
        return SearchResult {
            best_move: Some(root_moves[0]),
            ponder_move: None,
            nodes: 0,
            score: None,
        };
    }

    let (main_td, helper_tds) = threads.data.split_at_mut(1);
    let (main, nodes) = thread::scope(|scope| {
        let helpers: Vec<_> = helper_tds
            .iter_mut()
            .enumerate()
            .filter_map(|(i, thread_data)| {
                let helper_control = Arc::clone(control);
                thread::Builder::new()
                    .name(format!("search-helper-{}", i + 1))
                    .stack_size(SEARCH_THREAD_STACK)
                    .spawn_scoped(scope, move || {
                        iterative_deepening(
                            board,
                            tt,
                            thread_data,
                            limits,
                            helper_control,
                            ThreadRole::Helper,
                        )
                        .nodes
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
                    limits,
                    Arc::clone(&main_control),
                    ThreadRole::Main(reporter),
                );
                if limits.wait_for_stop {
                    wait_until_released(&main_control);
                }
                main_control.stop();
                result
            });

        let failed = || ThreadOutcome {
            best_move: None,
            nodes: 0,
            score: None,
        };
        let main = if let Ok(handle) = main {
            handle.join().unwrap_or_else(|_| failed())
        } else {
            control.stop();
            failed()
        };
        let helper_nodes: u64 = helpers.into_iter().map(|h| h.join().unwrap_or(0)).sum();
        let nodes = main.nodes + helper_nodes;
        (main, nodes)
    });

    let best_move = main.best_move.or(Some(root_moves[0]));
    let ponder_move = best_move.and_then(|m| ponder_move_after(board, m, tt));
    SearchResult {
        best_move,
        ponder_move,
        nodes,
        score: main.score.map(Score::from_search),
    }
}

#[cfg(test)]
mod time_tests {
    use super::*;
    use crate::types::Square;

    /// An untimed search's control whose clock read `ms` when it began.
    fn untimed_control_started_ms_ago(ms: u64) -> SearchControl {
        let control = Arc::into_inner(SearchControl::new(None)).expect("sole owner");
        SearchControl {
            start: Instant::now()
                .checked_sub(Duration::from_millis(ms))
                .expect("the clock has run longer than the test's lead"),
            ..control
        }
    }

    fn iteration(best_move_node_share: f64, score_drop: i32) -> IterationSummary {
        IterationSummary {
            best_move: Some(Move::quiet(Square::new(12), Square::new(28))),
            best_move_node_share,
            score_drop,
        }
    }

    const BUDGET: TimeBudget = TimeBudget {
        soft_ms: 1000,
        hard_ms: 4000,
    };
    const PONDER_MS: u64 = 8000;

    #[test]
    fn a_fresh_search_keeps_iterating_right_after_it_starts() {
        let control = untimed_control_started_ms_ago(0);
        control.set_budget_from_now(BUDGET);
        let mut time = TimeManager::new(Arc::new(control), true, None);

        let stop = time.should_stop_iterating(10, &iteration(0.9, 0));

        assert!(!stop);
    }

    #[test]
    fn a_search_keeps_iterating_right_after_ponderhit() {
        let control = untimed_control_started_ms_ago(PONDER_MS);
        control.set_budget_from_now(BUDGET);
        let mut time = TimeManager::new(Arc::new(control), true, None);

        let stop = time.should_stop_iterating(10, &iteration(0.9, 0));

        assert!(
            !stop,
            "stopped right after ponderhit with a 1000 ms soft budget"
        );
    }

    #[test]
    fn a_search_stops_iterating_three_soft_budgets_after_ponderhit() {
        let mut control = untimed_control_started_ms_ago(PONDER_MS);
        control.set_budget_from_now(BUDGET);
        control.start -= Duration::from_millis(3 * BUDGET.soft_ms);
        let mut time = TimeManager::new(Arc::new(control), true, None);

        let stop = time.should_stop_iterating(10, &iteration(0.0, SCORE_DROP_CAP));

        assert!(
            stop,
            "still iterating 3000 ms after ponderhit with a 1000 ms soft budget"
        );
    }
}

#[cfg(test)]
mod mate_score_tests {
    use super::*;

    #[test]
    fn a_tt_score_reads_back_as_stored_at_the_same_ply() {
        let scores = [
            0,
            250,
            -250,
            MATE_THRESHOLD,
            -MATE_THRESHOLD,
            MATE_THRESHOLD + 1,
            -MATE_THRESHOLD - 1,
            MATE_SCORE - 1,
            -MATE_SCORE + 1,
        ];
        for score in scores {
            for ply in [0, 7, 120] {
                assert_eq!(
                    score_from_tt(score_to_tt(score, ply), ply),
                    score,
                    "{score} at {ply}"
                );
            }
        }
    }

    #[test]
    fn a_mate_keeps_its_distance_from_the_node_whatever_ply_reads_it() {
        let mate_two_below_ply_3 = MATE_SCORE - (3 + 2);
        let mated_two_below_ply_3 = -MATE_SCORE + (3 + 2);

        let stored = [mate_two_below_ply_3, mated_two_below_ply_3].map(|s| score_to_tt(s, 3));
        let read_at_ply_10 = stored.map(|s| score_from_tt(s, 10));

        assert_eq!(
            read_at_ply_10,
            [MATE_SCORE - (10 + 2), -MATE_SCORE + (10 + 2)]
        );
    }

    #[test]
    fn scores_short_of_mate_pass_through_the_tt_unchanged() {
        for score in [MATE_THRESHOLD, -MATE_THRESHOLD, 0] {
            assert_eq!(score_to_tt(score, 50), score);
            assert_eq!(score_from_tt(score, 50), score);
        }
    }

    #[test]
    fn a_search_score_is_reported_as_moves_to_mate_or_centipawns() {
        let reported = [
            MATE_SCORE - 1,
            MATE_SCORE - 3,
            -MATE_SCORE,
            -MATE_SCORE + 2,
            cp_to_search_units(300),
        ]
        .map(Score::from_search);

        assert_eq!(
            reported,
            [
                Score::MateIn(1),
                Score::MateIn(2),
                Score::MateIn(0),
                Score::MateIn(-1),
                Score::Centipawns(300),
            ]
        );
    }
}
