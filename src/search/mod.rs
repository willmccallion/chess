use crate::board::Board;
use crate::eval::evaluate;
use crate::history::{ContinuationKey, CorrectionHistory, Histories, captured_kind, stat_bonus};
use crate::legality::LegalityInfo;
use crate::movepicker::{MovePicker, OrderingContext, ScoredMoves};
use crate::search_params::SearchParams;
use crate::see::{PIECE_VALUES, see, see_ge};
use crate::time::TimeBudget;
use crate::tt::{Bound, SharedTransTable, TTEntry, TTWrite};
use crate::types::{Centipawns, Move, MoveList, Piece, StackVec, Undo, ZKey};
use crate::uci_io::format_uci;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

mod depth;
mod score;
pub use depth::Depth;
pub use score::{SearchScore, TtScore};

const MAX_PLY: usize = 128;

const ASPIRATION_MIN_DEPTH: Depth = Depth::new(4);
const MAX_DOUBLE_EXTENSIONS: u8 = 6;
/// A singular move searched two plies deeper.
const DOUBLE_EXTENSION: Depth = Depth::new(2);
/// How much shallower than the node a TT entry may be and still rule out `ProbCut`.
const PROBCUT_TT_DEPTH_MARGIN: Depth = Depth::new(3);
/// How far a reduced search must beat the best score, per ply of depth, beyond
/// `deeper_margin`, to be searched again a ply deeper.
const DEEPER_MARGIN_PER_PLY: SearchScore = SearchScore::new(2);

const MAX_TRIED_MOVES: usize = 64;

const LMR_TABLE_SIZE: usize = 64;

const SEARCH_THREAD_STACK: usize = 32 * 1024 * 1024;
const NO_DEADLINE: u64 = u64::MAX;
const TIME_CHECK_INTERVAL: u64 = 2048;
const NEW_ITERATION_FRACTION: f64 = 0.6;
const NODE_SHARE_PIVOT: f64 = 1.6;
const NODE_SHARE_MIN_SCALE: f64 = 0.6;
const NODE_SHARE_MAX_SCALE: f64 = 1.6;
const SCORE_DROP_CAP: SearchScore = SearchScore::new(100);
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
        let (soft, hard) = budget.map_or((NO_DEADLINE, NO_DEADLINE), |b| {
            (b.soft.left(), b.hard.left())
        });
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
            .store(now + budget.soft.left(), Ordering::Relaxed);
        // Release: a search that sees the deadline sees the start and soft
        // deadline stored before it.
        self.hard_deadline_ms
            .store(now + budget.hard.left(), Ordering::Release);
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
    pub max_depth: Depth,
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
    score: Option<SearchScore>,
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
    fn base_reduction(&self, depth: Depth, moves_searched: i32, is_quiet: bool) -> i32 {
        let d = (depth.plies().max(0) as usize).min(LMR_TABLE_SIZE - 1);
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
    fn should_stop_iterating(&mut self, depth: Depth, iteration: &IterationSummary) -> bool {
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
        } else if depth > Depth::new(8) && self.best_move_stability >= 4 {
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
    score_drop: SearchScore,
}

/// Spend less time when one move absorbs most of the search, more when it is contested.
fn node_share_scale(best_move_node_share: f64) -> f64 {
    (NODE_SHARE_PIVOT - best_move_node_share).clamp(NODE_SHARE_MIN_SCALE, NODE_SHARE_MAX_SCALE)
}

/// Spend more time when the evaluation is falling.
fn score_drop_scale(score_drop: SearchScore) -> f64 {
    1.0 + f64::from(score_drop.clamp(SearchScore::ZERO, SCORE_DROP_CAP).units())
        / SCORE_DROP_DIVISOR
}

/// What the search knows about one ply of the current line.
#[derive(Copy, Clone)]
struct StackEntry {
    static_eval: Option<SearchScore>,
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
    root_best: Option<(Move, SearchScore)>,
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
    board.halfmove_clock() >= 100 && (!board.in_check() || board.has_legal_move())
}

/// Makes `m` if it is legal in `pos`.
fn make_if_legal(pos: &mut Board, m: Move) -> Option<Undo> {
    let info = pos.legality_info();
    (pos.is_pseudo_legal(m) && pos.is_legal(m, &info)).then(|| pos.make_move(m))
}

/// Piece-index for the counter-move table: the piece that made `prev` now stands on `prev.to`.
#[inline(always)]
const fn counter_move_slot(board: &Board, prev: Move) -> (usize, usize, usize) {
    (
        prev.is_capture() as usize,
        board.piece_on()[prev.to.index()].index(),
        prev.to.index(),
    )
}

#[inline(always)]
fn tt_cutoff_allowed(
    bound: Bound,
    score: SearchScore,
    alpha: SearchScore,
    beta: SearchScore,
) -> bool {
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
fn bound_beyond(bound: Bound, score: SearchScore, eval: SearchScore) -> bool {
    match bound {
        Bound::Exact => true,
        Bound::Lower => score > eval,
        Bound::Upper => score < eval,
    }
}

/// `eval`, or the TT entry's score where its bound places the position's
/// value past `eval`.
#[inline(always)]
fn refined_eval(entry: Option<TTEntry>, eval: SearchScore, ply: usize) -> SearchScore {
    match entry {
        Some(entry) if bound_beyond(entry.bound(), entry.score().at_ply(ply), eval) => {
            entry.score().at_ply(ply)
        }
        _ => eval,
    }
}

#[inline(always)]
fn bound_for(best_score: SearchScore, alpha_orig: SearchScore, beta: SearchScore) -> Bound {
    if best_score <= alpha_orig {
        Bound::Upper
    } else if best_score >= beta {
        Bound::Lower
    } else {
        Bound::Exact
    }
}

const fn has_non_pawn_material(board: &Board) -> bool {
    let our_pieces = board.pieces_of(board.turn());
    let pawns_and_kings = board.piece_bb()[Piece::WP.index()]
        | board.piece_bb()[Piece::BP.index()]
        | board.piece_bb()[Piece::WK.index()]
        | board.piece_bb()[Piece::BK.index()];
    our_pieces & !pawns_and_kings != 0
}

impl Search<'_> {
    /// Static eval is better than two plies ago (four if that node was in check).
    fn is_improving(&self, static_eval: Option<SearchScore>) -> bool {
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

    /// Starts the node's stack entry: the double extensions on its path, and
    /// whether it is or was on a PV.
    fn start_stack_entry(&mut self, is_pv: bool, tt_entry: Option<TTEntry>) {
        let inherited = match self.ply.checked_sub(1) {
            Some(parent) => self.stack[parent].double_extensions,
            None => 0,
        };
        let entry = &mut self.stack[self.ply];
        entry.double_extensions = inherited;
        entry.tt_pv = is_pv || tt_entry.is_some_and(|e| e.is_pv());
    }

    /// Clears what the node's children and grandchildren would inherit from
    /// the last node searched at their plies.
    fn clear_child_slots(&mut self) {
        self.killers[(self.ply + 1).min(MAX_PLY - 1)] = [None; 2];
        self.stack[(self.ply + 2).min(MAX_PLY)].cutoff_count = 0;
    }

    /// The history score of `m` if it is quiet, else 0.
    fn quiet_history_of(&self, m: Move, continuations: &[Option<ContinuationKey>; 2]) -> i32 {
        if !m.is_quiet() {
            return 0;
        }
        let piece = self.board.piece_on()[m.from.index()];
        self.thread_data
            .histories
            .quiet_history(self.board.turn(), piece, m, continuations)
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
    fn static_eval(&mut self, tt_eval: Option<SearchScore>) -> SearchScore {
        tt_eval.unwrap_or_else(|| SearchScore::from_centipawns(evaluate(&mut self.board)))
    }

    /// Static eval adjusted by what searches have learned about this pawn structure.
    fn corrected_eval(&self, raw_eval: SearchScore) -> SearchScore {
        let correction = self
            .thread_data
            .correction
            .correction(self.board.turn(), self.board.pawn_key());
        (raw_eval + correction).clamp(
            -SearchScore::MATE_THRESHOLD + SearchScore::UNIT,
            SearchScore::MATE_THRESHOLD - SearchScore::UNIT,
        )
    }

    /// Makes `m` (pseudo-legal in the current position) unless it leaves the king in check.
    fn make_move(&mut self, m: Move, legality: &LegalityInfo) -> Option<Undo> {
        if !self.board.is_legal(m, legality) {
            return None;
        }
        let moved_piece = self.board.piece_on()[m.from.index()];
        let undo = self.board.make_move(m);
        self.tt.prefetch(self.board.zobrist());
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
    fn window_below_root(
        &mut self,
        alpha: SearchScore,
        beta: SearchScore,
    ) -> ControlFlow<SearchScore, (SearchScore, SearchScore)> {
        if self.board.is_repetition(self.ply) || is_fifty_move_draw(&self.board) {
            return ControlFlow::Break(SearchScore::DRAW);
        }
        if self.ply >= MAX_PLY - 1 {
            return ControlFlow::Break(SearchScore::from_centipawns(evaluate(&mut self.board)));
        }
        let alpha = alpha.max(SearchScore::mated_at(self.ply));
        let beta = beta.min(SearchScore::mate_at(self.ply + 1));
        if alpha >= beta {
            return ControlFlow::Break(alpha);
        }
        ControlFlow::Continue((alpha, beta))
    }

    /// Whether `m` is a capture quiescence skips: one that loses material, or
    /// one whose victim, with a margin, cannot lift `stand_pat` to `alpha`.
    /// The victim's SEE value, near centipawns, is added to a score in search
    /// units as it is; `delta_margin` was tuned that way.
    fn is_futile_capture(&self, m: Move, stand_pat: SearchScore, alpha: SearchScore) -> bool {
        if !m.is_capture() {
            return false;
        }
        let victim_falls_short = m.promotion().is_none()
            && self.board.piece_on()[m.to.index()]
                .kind()
                .is_some_and(|kind| {
                    stand_pat
                        + SearchScore::unscaled(PIECE_VALUES[kind as usize])
                        + self.params.delta_margin
                        < alpha
                });
        victim_falls_short || !see_ge(&self.board, m, Centipawns::ZERO)
    }

    /// The TT's score for this node when `entry` is deep enough and its bound
    /// settles the window, crediting the TT move when that score fails high.
    fn tt_cutoff(
        &mut self,
        entry: TTEntry,
        depth: Depth,
        alpha: SearchScore,
        beta: SearchScore,
    ) -> Option<SearchScore> {
        let tt_score = entry.score().at_ply(self.ply);
        if entry.depth() < depth || !tt_cutoff_allowed(entry.bound(), tt_score, alpha, beta) {
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
    fn update_correction(&mut self, static_eval: SearchScore, outcome: NodeOutcome) {
        if outcome
            .best_move
            .is_some_and(super::types::Move::is_capture)
            || outcome.best_score.abs() >= SearchScore::MATE_THRESHOLD
            || !bound_beyond(outcome.bound, outcome.best_score, static_eval)
        {
            return;
        }
        let pawn_key = self.board.pawn_key();
        self.thread_data.correction.update(
            self.board.turn(),
            pawn_key,
            outcome.best_score - static_eval,
            outcome.depth,
            self.params.correction_max_weight,
        );
    }

    /// Credits a quiet TT move whose stored score still fails high.
    fn reward_tt_cutoff_move(&mut self, m: Move, depth: Depth) {
        if !m.is_quiet() || !self.board.is_pseudo_legal(m) {
            return;
        }
        let piece = self.board.piece_on()[m.from.index()];
        let continuations = self.continuation_keys();
        self.thread_data.histories.update_quiet(
            self.board.turn(),
            m,
            piece,
            &continuations,
            stat_bonus(depth, &self.params),
        );
    }

    /// The opponent's last move refuted everything here, so it was a good quiet move.
    fn reward_prior_quiet_move(&mut self, depth: Depth) {
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
            self.board.turn().other(),
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
        let piece = self.board.piece_on()[m.from.index()];
        if let Some(victim) = captured_kind(self.board.piece_on()[m.to.index()], m) {
            self.thread_data
                .histories
                .update_capture(piece, m.to, victim, bonus);
        }
    }

    /// Rewards the move that caused a beta cutoff and penalizes the moves tried before it.
    fn update_cutoff_stats(&mut self, best: Move, depth: Depth, tried: &TriedMoves) {
        let bonus = stat_bonus(depth, &self.params);
        let continuations = self.continuation_keys();
        let color = self.board.turn();

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
            let piece = self.board.piece_on()[best.from.index()];
            self.thread_data
                .histories
                .update_quiet(color, best, piece, &continuations, bonus);
            for &quiet in tried.quiets.as_slice() {
                let piece = self.board.piece_on()[quiet.from.index()];
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

/// Searches captures until the position is quiet, standing pat on the static
/// evaluation; every evasion when in check.
fn quiesce(
    search: &mut Search,
    alpha: SearchScore,
    beta: SearchScore,
) -> Result<SearchScore, Aborted> {
    search.seldepth = search.seldepth.max(search.ply);
    search.time.count_node();
    if search.time.check_hard_limit() {
        return Err(Aborted);
    }
    if search.ply >= MAX_PLY - 1 {
        return Ok(SearchScore::from_centipawns(evaluate(&mut search.board)));
    }

    let is_pv = beta - alpha > SearchScore::UNIT;
    let key = search.board.zobrist();
    let tt_entry = search.tt.probe(key);
    if let Some(entry) = tt_entry {
        let tt_score = entry.score().at_ply(search.ply);
        if !is_pv && tt_cutoff_allowed(entry.bound(), tt_score, alpha, beta) {
            return Ok(tt_score);
        }
    }

    let legality = search.board.legality_info();
    if legality.in_check() {
        return quiesce_evasions(search, alpha, beta, &legality);
    }

    let node_eval = search.evaluate_node(tt_entry);
    let ply = search.ply;
    let entry = |score: SearchScore, bound, best_move| TTWrite {
        depth: Depth::ZERO,
        score: score.to_tt(ply),
        eval: Some(node_eval.raw),
        bound,
        best_move,
        is_pv: tt_entry.is_some_and(|e| e.is_pv()),
    };
    let stand_pat = node_eval.refined;
    if stand_pat >= beta {
        if tt_entry.is_none() {
            search.tt.store(key, entry(stand_pat, Bound::Lower, None));
        }
        return Ok(stand_pat);
    }

    let bounds = NodeBounds {
        alpha: alpha.max(stand_pat),
        beta,
        best_score: stand_pat,
    };
    let (best_score, best_move) = quiesce_captures(search, &legality, bounds)?;
    let bound = bound_for(best_score, alpha, beta);
    search.tt.store(key, entry(best_score, bound, best_move));
    Ok(best_score)
}

/// Searches the captures quiescence does not prune, from `bounds` with the
/// stand-pat score as the best so far; the best score and the move that
/// raised alpha to it.
fn quiesce_captures(
    search: &mut Search,
    legality: &LegalityInfo,
    bounds: NodeBounds,
) -> Result<(SearchScore, Option<Move>), Aborted> {
    use crate::movepicker::QMovePicker;

    let NodeBounds {
        mut alpha,
        beta,
        best_score: stand_pat,
    } = bounds;
    let mut best_score = stand_pat;
    let mut best_move = None;
    let mut picker = QMovePicker::new();
    while let Some(m) = picker.next(&search.board) {
        if search.is_futile_capture(m, stand_pat, alpha) {
            continue;
        }
        let Some(score) = quiesce_move(search, m, legality, alpha, beta)? else {
            continue;
        };
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
    Ok((best_score, best_move))
}

/// Quiescence when in check: every evasion is searched so mates are found.
fn quiesce_evasions(
    search: &mut Search,
    mut alpha: SearchScore,
    beta: SearchScore,
    legality: &LegalityInfo,
) -> Result<SearchScore, Aborted> {
    let mut evasions = ScoredMoves::new();
    evasions.fill(
        |moves| search.board.generate_pseudo_legal_moves(moves),
        |m| {
            if m.is_capture() {
                see(&search.board, m).get()
            } else {
                0
            }
        },
    );

    let mut best_score = SearchScore::mated_at(search.ply);
    while let Some(m) = evasions.pick_best() {
        let Some(score) = quiesce_move(search, m, legality, alpha, beta)? else {
            continue;
        };
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

/// The quiescence score of `m` for the side making it, in the window
/// `alpha`..`beta`; `None` when `m` leaves its king in check.
fn quiesce_move(
    search: &mut Search,
    m: Move,
    legality: &LegalityInfo,
    alpha: SearchScore,
    beta: SearchScore,
) -> Result<Option<SearchScore>, Aborted> {
    let Some(undo) = search.make_move(m, legality) else {
        return Ok(None);
    };
    let score = quiesce(search, -beta, -alpha);
    search.unmake_move(m, undo);
    let score = -score?;
    if search.time.check_hard_limit() {
        return Err(Aborted);
    }
    Ok(Some(score))
}

/// The score that ends a non-PV node not in check before its moves: from
/// razoring, reverse futility or null-move pruning, or else from `ProbCut`.
fn prune_before_moves(search: &mut Search, n: &Node) -> Result<Option<SearchScore>, Aborted> {
    let Some(eval) = n.eval.filter(|_| !n.is_pv) else {
        return Ok(None);
    };
    if let Some(score) = prune_node(search, n, eval.refined)? {
        return Ok(Some(score));
    }
    if n.excluded_move.is_some() {
        return Ok(None);
    }
    probcut(search, n, eval.corrected)
}

/// The score a node returns without searching its moves, when its static
/// evaluation `eval` is far enough outside the window.
fn prune_node(
    search: &mut Search,
    n: &Node,
    eval: SearchScore,
) -> Result<Option<SearchScore>, Aborted> {
    let razor_margin =
        search.params.razor_margin + search.params.razor_depth_sq_margin * n.depth.plies().pow(2);
    if eval < n.alpha - razor_margin {
        let q_score = quiesce(search, n.alpha, n.beta)?;
        if q_score < n.alpha {
            return Ok(Some(q_score));
        }
    }
    if n.depth <= search.params.rfp_max_depth
        && eval - search.params.rfp_margin * (n.depth.plies() - i32::from(n.improving)) >= n.beta
    {
        return Ok(Some(eval));
    }
    null_move_cutoff(search, n, eval)
}

/// Passes the move and searches the reply shallower; a side that still fails
/// high after giving its opponent a free move fails high here too.
fn null_move_cutoff(
    search: &mut Search,
    n: &Node,
    eval: SearchScore,
) -> Result<Option<SearchScore>, Aborted> {
    let previous_was_null = search.ply > 0 && search.stack[search.ply - 1].current_move.is_none();
    if n.depth < search.params.nmp_min_depth
        || n.excluded_move.is_some()
        || eval < n.beta
        || previous_was_null
        || !has_non_pawn_material(&search.board)
    {
        return Ok(None);
    }
    let r = search.params.nmp_base_reduction
        + n.depth / search.params.nmp_depth_divisor
        + Depth::new((eval - n.beta) / search.params.nmp_eval_divisor)
            .min(search.params.nmp_eval_max);
    let undo = search.make_null_move();
    let null_score = negamax(
        search,
        -n.beta,
        -n.beta + SearchScore::UNIT,
        n.depth - r,
        n.kind.opposite(),
    );
    search.unmake_null_move(undo);
    let null_score = -null_score?;
    if search.time.check_hard_limit() {
        return Err(Aborted);
    }
    if null_score < n.beta {
        return Ok(None);
    }
    // A mate found after passing proves nothing about the real moves.
    Ok(Some(if null_score > SearchScore::MATE_THRESHOLD {
        n.beta
    } else {
        null_score
    }))
}

/// Looks for a capture that beats beta by a margin in quiescence and then in a
/// reduced search; if one exists the node almost certainly fails high at full
/// depth too. Skipped near mate scores and where the TT entry already falls short.
fn probcut(
    search: &mut Search,
    n: &Node,
    static_eval: SearchScore,
) -> Result<Option<SearchScore>, Aborted> {
    use crate::movepicker::QMovePicker;

    if n.depth < search.params.probcut_min_depth || n.beta.abs() >= SearchScore::MATE_THRESHOLD {
        return Ok(None);
    }
    let probcut_beta = n.beta + search.params.probcut_margin;
    let tt_rules_out_probcut = n.tt_entry.is_some_and(|entry| {
        entry.depth() >= n.depth - PROBCUT_TT_DEPTH_MARGIN
            && entry.score().at_ply(search.ply) < probcut_beta
    });
    if tt_rules_out_probcut {
        return Ok(None);
    }
    let probe_depth = n.depth - search.params.probcut_reduction;
    let mut picker = QMovePicker::new();
    while let Some(m) = picker.next(&search.board) {
        if !see_ge(
            &search.board,
            m,
            (probcut_beta - static_eval).as_unscaled_centipawns(),
        ) {
            continue;
        }
        let Some(undo) = search.make_move(m, &n.legality) else {
            continue;
        };
        let score = probe_capture(search, probcut_beta, probe_depth, n.kind);
        search.unmake_move(m, undo);
        let score = score?;

        if search.time.check_hard_limit() {
            return Err(Aborted);
        }
        if score >= probcut_beta {
            search.tt.store(
                search.board.zobrist(),
                TTWrite {
                    depth: probe_depth + Depth::ONE,
                    score: score.to_tt(search.ply),
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
    probcut_beta: SearchScore,
    probe_depth: Depth,
    node: NodeKind,
) -> Result<SearchScore, Aborted> {
    let q_score = -quiesce(search, -probcut_beta, -probcut_beta + SearchScore::UNIT)?;
    if q_score < probcut_beta {
        return Ok(q_score);
    }
    Ok(-negamax(
        search,
        -probcut_beta,
        -probcut_beta + SearchScore::UNIT,
        probe_depth,
        node.opposite(),
    )?)
}

enum Singularity {
    /// Search the move this much deeper (negative: shallower).
    Extend(Depth),
    /// Even without the TT move the node fails high; return this score.
    MultiCut(SearchScore),
}

/// How much deeper to search `m` from `node`: when `m` is the TT move of a
/// node below the root, whether it is singular, found by searching the node
/// without it.
fn singular_extension(search: &mut Search, node: &Node, m: Move) -> Result<Singularity, Aborted> {
    let Some(entry) = node
        .tt_entry
        .filter(|_| Some(m) == node.tt_move && !node.is_root && node.excluded_move.is_none())
    else {
        return Ok(Singularity::Extend(Depth::ZERO));
    };
    let depth = node.depth;
    let tt_score = entry.score().at_ply(search.ply);
    let reliable = depth >= search.params.singular_min_depth
        && entry.depth() >= depth - search.params.singular_tt_depth_margin
        && entry.bound() != Bound::Upper
        && tt_score.abs() < SearchScore::MATE_THRESHOLD;
    if !reliable {
        return Ok(Singularity::Extend(Depth::ZERO));
    }

    let singular_beta = tt_score - search.params.singular_beta_per_depth_x16 * depth.plies() / 16;
    let singular_depth = (depth - Depth::ONE) / 2;
    search.stack[search.ply].excluded_move = Some(m);
    let singular_score = negamax(
        search,
        singular_beta - SearchScore::UNIT,
        singular_beta,
        singular_depth,
        node.kind,
    );
    search.stack[search.ply].excluded_move = None;
    let singular_score = singular_score?;
    if search.time.check_hard_limit() {
        return Err(Aborted);
    }

    Ok(if singular_score < singular_beta {
        let double = !node.is_pv
            && singular_score < singular_beta - search.params.double_extension_margin
            && search.stack[search.ply].double_extensions < MAX_DOUBLE_EXTENSIONS;
        Singularity::Extend(if double { DOUBLE_EXTENSION } else { Depth::ONE })
    } else if singular_beta >= node.beta {
        Singularity::MultiCut(singular_beta)
    } else if tt_score >= node.beta {
        Singularity::Extend(Depth::new(-1))
    } else {
        Singularity::Extend(Depth::ZERO)
    })
}

/// What the move loop does with a move before searching it.
enum Pruning {
    Search,
    Skip,
    /// Skip it and every quiet move after it.
    SkipQuiets,
}

impl Search<'_> {
    /// Applies late-move, history, futility and SEE pruning to the candidate
    /// `c`; never at the root, in check, to the first move or once every move
    /// so far loses to mate.
    fn prune_move(&self, node: &Node, state: &MoveLoop, c: &Candidate) -> Pruning {
        let Some(eval) = node.eval.map(|e| e.refined) else {
            return Pruning::Search;
        };
        if node.is_root
            || state.moves_searched == 0
            || state.best_score <= -SearchScore::MATE_THRESHOLD
        {
            return Pruning::Search;
        }
        let params = &self.params;
        let plies = node.depth.plies();
        let shallow = node.depth <= params.move_pruning_max_depth;
        if !c.is_quiet {
            let losing = shallow && !see_ge(&self.board, c.m, -params.see_capture_margin * plies);
            return if losing {
                Pruning::Skip
            } else {
                Pruning::Search
            };
        }
        if state.quiets_seen > state.lmp_limit {
            return Pruning::SkipQuiets;
        }
        if node.depth <= params.history_pruning_max_depth
            && c.quiet_history < -params.history_pruning_margin * plies
        {
            return Pruning::Skip;
        }
        if shallow && eval + params.futility_base + params.futility_per_depth * plies <= state.alpha
        {
            return Pruning::SkipQuiets;
        }
        if shallow && !see_ge(&self.board, c.m, -params.see_quiet_margin * plies.pow(2)) {
            return Pruning::Skip;
        }
        Pruning::Search
    }
}

/// The static evaluations of a node not in check.
#[derive(Clone, Copy)]
struct NodeEval {
    /// The evaluation itself, which the TT keeps.
    raw: SearchScore,
    /// `raw` adjusted by what searches learned about the pawn structure.
    corrected: SearchScore,
    /// `corrected`, or the TT score where its bound places the value past it.
    refined: SearchScore,
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
    depth: Depth,
    best_score: SearchScore,
    best_move: Option<Move>,
    bound: Bound,
}

/// A node's window and the best score its moves have reached so far.
#[derive(Clone, Copy)]
struct NodeBounds {
    alpha: SearchScore,
    beta: SearchScore,
    best_score: SearchScore,
}

/// A move made on the board from `node`, about to be searched.
struct MadeMove<'n> {
    node: &'n Node,
    candidate: Candidate,
    /// The depth the move is searched to before any reduction.
    new_depth: Depth,
    /// Which legal move of the node this is, from 1.
    moves_searched: i32,
}

/// Searches a move already made: the first with the node's full window,
/// later ones through [`search_late_move`].
fn search_made_move(
    search: &mut Search,
    m: &MadeMove,
    state: &MoveLoop,
) -> Result<SearchScore, Aborted> {
    let node = m.node;
    if m.moves_searched == 1 {
        let child = node.kind.first_child(node.is_pv);
        return Ok(-negamax(
            search,
            -node.beta,
            -state.alpha,
            m.new_depth,
            child,
        )?);
    }
    let bounds = NodeBounds {
        alpha: state.alpha,
        beta: node.beta,
        best_score: state.best_score,
    };
    search_late_move(search, m, bounds)
}

/// Searches a move after the first: with a null window at reduced depth,
/// again at full depth if it beats alpha, and with the full window if it
/// then lands inside a PV node's window.
fn search_late_move(
    search: &mut Search,
    m: &MadeMove,
    bounds: NodeBounds,
) -> Result<SearchScore, Aborted> {
    let NodeBounds {
        alpha,
        beta,
        best_score,
    } = bounds;
    let reduction = search.late_move_reduction(m);
    let reduced_depth = m.new_depth - reduction;
    let mut score = -negamax(
        search,
        -alpha - SearchScore::UNIT,
        -alpha,
        reduced_depth,
        NodeKind::Cut,
    )?;
    if score > alpha && reduction > Depth::ZERO {
        let deeper = score
            > best_score
                + search.params.deeper_margin
                + DEEPER_MARGIN_PER_PLY * m.new_depth.plies();
        let shallower = score < best_score + search.params.shallower_margin;
        let research_depth = m.new_depth + Depth::new(i32::from(deeper) - i32::from(shallower));
        if research_depth > reduced_depth {
            score = -negamax(
                search,
                -alpha - SearchScore::UNIT,
                -alpha,
                research_depth,
                m.node.kind.opposite(),
            )?;
        }
    }
    if m.node.is_pv && score > alpha && score < beta {
        score = -negamax(search, -beta, -alpha, m.new_depth, NodeKind::All)?;
    }
    Ok(score)
}

impl Search<'_> {
    /// How many plies to reduce a late move by, with the move made on the board.
    fn late_move_reduction(&self, m: &MadeMove) -> Depth {
        let (node, c) = (m.node, &m.candidate);
        let params = &self.params;
        if node.depth < params.lmr_min_depth || node.in_check || (node.is_pv && !c.is_quiet) {
            return Depth::ZERO;
        }
        let child_cutoffs = i32::from(self.stack[self.ply].cutoff_count);
        let adjustments_1024 = [
            (!node.is_pv, params.lmr_non_pv_x1024),
            (node.kind == NodeKind::Cut, params.lmr_cut_node_x1024),
            (!node.improving, params.lmr_not_improving_x1024),
            (c.killer_or_counter, -params.lmr_killer_or_counter_x1024),
            (self.board.in_check(), -params.lmr_gives_check_x1024),
            (
                node.tt_move.is_some_and(Move::is_capture),
                params.lmr_tt_capture_x1024,
            ),
            (node.tt_pv, -params.lmr_tt_pv_x1024),
            (
                child_cutoffs > params.child_cutoff_threshold,
                params.lmr_child_cutoffs_x1024,
            ),
        ];
        let reduction_1024 = 1024
            * self
                .lmr
                .base_reduction(node.depth, m.moves_searched, c.is_quiet)
            + adjustments_1024
                .iter()
                .filter(|&&(applies, _)| applies)
                .map(|&(_, adjustment)| adjustment)
                .sum::<i32>();
        let r = reduction_1024 / 1024 - c.quiet_history / params.lmr_history_divisor;
        Depth::new(r.clamp(0, (m.new_depth.plies() - 1).max(0)))
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
    alpha: SearchScore,
    beta: SearchScore,
    depth: Depth,
    kind: NodeKind,
) -> Result<SearchScore, Aborted> {
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
    depth: Depth,
    /// The window's lower bound before any move raised it.
    alpha: SearchScore,
    beta: SearchScore,
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
    mut alpha: SearchScore,
    mut beta: SearchScore,
    depth: Depth,
    kind: NodeKind,
) -> Result<ControlFlow<SearchScore, Node>, Aborted> {
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

    let is_pv = beta - alpha > SearchScore::UNIT;
    let key = search.board.zobrist();
    let excluded_move = search.stack[search.ply].excluded_move;
    let tt_entry = search.tt.probe(key);
    if excluded_move.is_none() {
        search.start_stack_entry(is_pv, tt_entry);
        if !is_pv
            && let Some(entry) = tt_entry
            && let Some(score) = search.tt_cutoff(entry, depth, alpha, beta)
        {
            return Ok(ControlFlow::Break(score));
        }
    }

    let legality = search.board.legality_info();
    let in_check = legality.in_check();
    let depth = if in_check { depth + Depth::ONE } else { depth };
    if depth <= Depth::ZERO {
        return quiesce(search, alpha, beta).map(ControlFlow::Break);
    }

    search.time.count_node();
    search.clear_child_slots();
    let eval = (!in_check).then(|| search.evaluate_node(tt_entry));
    let corrected_eval = eval.map(|e| e.corrected);
    search.stack[search.ply].static_eval = corrected_eval;

    let mut node = Node {
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
        tt_move: tt_entry.and_then(|e| e.best_move()),
        tt_pv: search.stack[search.ply].tt_pv,
        eval,
        improving: search.is_improving(corrected_eval),
        legality,
    };
    if let Some(score) = prune_before_moves(search, &node)? {
        return Ok(ControlFlow::Break(score));
    }
    if node.depth >= search.params.iir_min_depth
        && node.tt_move.is_none()
        && excluded_move.is_none()
    {
        node.depth -= Depth::ONE;
    }
    Ok(ControlFlow::Continue(node))
}

/// The move loop's running state.
struct MoveLoop {
    alpha: SearchScore,
    best_score: SearchScore,
    best_move: Option<Move>,
    moves_searched: i32,
    quiets_seen: i32,
    /// Quiet moves past this many are skipped.
    lmp_limit: i32,
    tried: TriedMoves,
}

impl MoveLoop {
    /// Takes a searched move's `score`; true when it fails high, after the
    /// cutoff has been credited.
    fn take_score(
        &mut self,
        search: &mut Search,
        node: &Node,
        m: Move,
        score: SearchScore,
    ) -> bool {
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
    Scored(SearchScore),
    /// The singularity search showed the node fails high without the move.
    MultiCut(SearchScore),
}

/// Searches the node's moves in the picker's order, pruning those the node
/// allows, until one fails high or none is left. Ends the node at once on
/// a multi-cut.
fn search_moves(
    search: &mut Search,
    node: &Node,
) -> Result<ControlFlow<SearchScore, MoveLoop>, Aborted> {
    let killers = search.killers[search.ply];
    let counter_move = search.counter_move();
    let mut picker = MovePicker::new(node.tt_move, killers[0], killers[1], counter_move);
    let continuations = search.continuation_keys();
    let mut state = MoveLoop {
        alpha: node.alpha,
        best_score: -SearchScore::BOUND,
        best_move: None,
        moves_searched: 0,
        quiets_seen: 0,
        lmp_limit: (search.params.lmp_base + node.depth.plies().pow(2))
            / (2 - i32::from(node.improving)),
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
        let c = Candidate {
            m,
            is_quiet: m.is_quiet(),
            quiet_history: search.quiet_history_of(m, &continuations),
            killer_or_counter: killers.contains(&Some(m)) || Some(m) == counter_move,
        };
        state.quiets_seen += i32::from(c.is_quiet);
        match search.prune_move(node, &state, &c) {
            Pruning::Search => {}
            Pruning::Skip => continue,
            Pruning::SkipQuiets => {
                picker.skip_quiets();
                continue;
            }
        }

        let score = match search_move(search, node, &mut state, c)? {
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

/// Searches one move, extended if singular.
fn search_move(
    search: &mut Search,
    node: &Node,
    state: &mut MoveLoop,
    c: Candidate,
) -> Result<MoveSearch, Aborted> {
    let extension = match singular_extension(search, node, c.m)? {
        Singularity::Extend(depth) => depth,
        Singularity::MultiCut(score) => return Ok(MoveSearch::MultiCut(score)),
    };
    let node_ply = search.ply;
    let nodes_before = search.time.nodes;
    let Some(undo) = search.make_move(c.m, &node.legality) else {
        return Ok(MoveSearch::Illegal);
    };
    state.moves_searched += 1;
    let is_double = extension >= DOUBLE_EXTENSION;
    if is_double {
        search.stack[node_ply].double_extensions += 1;
    }
    let made = MadeMove {
        node,
        candidate: c,
        new_depth: node.depth - Depth::ONE + extension,
        moves_searched: state.moves_searched,
    };
    let score = search_made_move(search, &made, state);
    search.unmake_move(c.m, undo);
    if is_double {
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
fn finish_node(search: &mut Search, node: &Node, searched: &MoveLoop) -> SearchScore {
    if searched.moves_searched == 0 {
        return if node.excluded_move.is_some() {
            searched.alpha
        } else if node.in_check {
            SearchScore::mated_at(search.ply)
        } else {
            SearchScore::DRAW
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
            depth: node.depth,
            score: searched.best_score.to_tt(search.ply),
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
        let Some(m) = tt.probe(pos.zobrist()).and_then(|e| e.best_move()) else {
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
    Centipawns(Centipawns),
    /// Positive when the side to move mates, negative when it is mated.
    MateIn(i32),
}

impl Score {
    fn from_search(score: SearchScore) -> Self {
        if score.abs() > SearchScore::MATE_THRESHOLD {
            let mate_in = (SearchScore::MATE.units() - score.abs().units() + 1) / 2;
            Self::MateIn(if score > SearchScore::DRAW {
                mate_in
            } else {
                -mate_in
            })
        } else {
            Self::Centipawns(score.to_centipawns())
        }
    }
}

/// What the main thread reports after each completed iteration.
#[derive(Clone, Debug)]
pub struct IterationInfo {
    pub depth: Depth,
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

fn iteration_info(
    search: &Search,
    depth: Depth,
    best_move: Move,
    score: SearchScore,
) -> IterationInfo {
    let elapsed_ms = search.time.control.elapsed_ms();
    let nodes = search.time.nodes_of_all_threads();
    let nps = (nodes * 1000).checked_div(elapsed_ms).unwrap_or(0);
    let mut after_best = search.board.clone();
    after_best.make_move(best_move);
    let pv = std::iter::once(best_move)
        .chain(get_pv_from_tt(
            after_best,
            search.tt,
            usize::try_from(depth.plies() - 1).unwrap_or(0),
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

    let mut score = SearchScore::ZERO;
    let mut completed_score = None;
    for depth in (1..=limits.max_depth.plies()).map(Depth::new) {
        search.seldepth = 0;
        let Some(iteration_score) = aspiration_search(&mut search, depth, score) else {
            break;
        };
        let previous_score = score;
        score = iteration_score;
        completed_score = Some(score);

        let best_move = search.root_best.map(|(m, _)| m);
        if let (ThreadRole::Main(report), Some(m)) = (role, best_move) {
            report(&iteration_info(&search, depth, m, score));
        }
        let summary = IterationSummary {
            best_move,
            best_move_node_share: best_move.map_or(0.0, |m| search.root_move_node_share(m)),
            score_drop: if depth > Depth::ONE {
                previous_score - score
            } else {
                SearchScore::ZERO
            },
        };
        if search.time.should_stop_iterating(depth, &summary) {
            break;
        }
        if score.abs() > SearchScore::MATE_THRESHOLD {
            break;
        }
    }

    ThreadOutcome {
        best_move: search.root_best.map(|(m, _)| m),
        nodes: search.time.nodes,
        score: completed_score,
    }
}

/// Searches the root to `depth` in a window around `previous`, the score of
/// the last depth, widening it after each fail until the score lands inside.
/// A fail high also searches a ply shallower. `None` if the search aborts.
fn aspiration_search(
    search: &mut Search,
    depth: Depth,
    previous: SearchScore,
) -> Option<SearchScore> {
    let mut delta = search.params.aspiration_delta;
    let (mut alpha, mut beta) = if depth >= ASPIRATION_MIN_DEPTH {
        (
            (previous - delta).max(-SearchScore::BOUND),
            (previous + delta).min(SearchScore::BOUND),
        )
    } else {
        (-SearchScore::BOUND, SearchScore::BOUND)
    };
    let mut fail_highs = 0;
    loop {
        let search_depth = (depth - Depth::new(fail_highs)).max(Depth::ONE);
        let iteration = negamax(search, alpha, beta, search_depth, NodeKind::All);
        let (Ok(score), false) = (iteration, search.time.check_hard_limit()) else {
            return None;
        };
        if score <= alpha {
            beta = alpha.midpoint(beta);
            alpha = (score - delta).max(-SearchScore::BOUND);
            fail_highs = 0;
        } else if score >= beta {
            beta = (score + delta).min(SearchScore::BOUND);
            fail_highs += 1;
        } else {
            return Some(score);
        }
        delta += delta / 2;
    }
}

/// The answer when there is nothing to search: no move when there is none
/// (after waiting for `stop` if asked to), or the only move when time is
/// short anyway.
fn answer_without_searching(
    root_moves: &[Move],
    limits: &SearchLimits,
    control: &SearchControl,
) -> Option<SearchResult> {
    let best_move = match root_moves {
        [] => {
            if limits.wait_for_stop {
                wait_until_released(control);
            }
            None
        }
        [only] if control.is_timed() && !limits.wait_for_stop => Some(*only),
        _ => return None,
    };
    Some(SearchResult {
        best_move,
        ponder_move: None,
        nodes: 0,
        score: None,
    })
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
    if let Some(answer) = answer_without_searching(root_moves, limits, control) {
        return answer;
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
    use crate::time::Millis;
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

    fn iteration(best_move_node_share: f64, score_drop: SearchScore) -> IterationSummary {
        IterationSummary {
            best_move: Some(Move::quiet(Square::new(12), Square::new(28))),
            best_move_node_share,
            score_drop,
        }
    }

    const BUDGET: TimeBudget = TimeBudget {
        soft: Millis::new(1000),
        hard: Millis::new(4000),
    };
    const PONDER_MS: u64 = 8000;

    #[test]
    fn a_fresh_search_keeps_iterating_right_after_it_starts() {
        let control = untimed_control_started_ms_ago(0);
        control.set_budget_from_now(BUDGET);
        let mut time = TimeManager::new(Arc::new(control), true, None);

        let stop = time.should_stop_iterating(Depth::new(10), &iteration(0.9, SearchScore::ZERO));

        assert!(!stop);
    }

    #[test]
    fn a_search_keeps_iterating_right_after_ponderhit() {
        let control = untimed_control_started_ms_ago(PONDER_MS);
        control.set_budget_from_now(BUDGET);
        let mut time = TimeManager::new(Arc::new(control), true, None);

        let stop = time.should_stop_iterating(Depth::new(10), &iteration(0.9, SearchScore::ZERO));

        assert!(
            !stop,
            "stopped right after ponderhit with a 1000 ms soft budget"
        );
    }

    #[test]
    fn a_search_stops_iterating_three_soft_budgets_after_ponderhit() {
        let mut control = untimed_control_started_ms_ago(PONDER_MS);
        control.set_budget_from_now(BUDGET);
        control.start -= Duration::from_millis(3 * BUDGET.soft.left());
        let mut time = TimeManager::new(Arc::new(control), true, None);

        let stop = time.should_stop_iterating(Depth::new(10), &iteration(0.0, SCORE_DROP_CAP));

        assert!(
            stop,
            "still iterating 3000 ms after ponderhit with a 1000 ms soft budget"
        );
    }
}

#[cfg(test)]
mod score_report_tests {
    use super::*;

    #[test]
    fn a_search_score_is_reported_as_moves_to_mate_or_centipawns() {
        let reported = [
            SearchScore::mate_at(1),
            SearchScore::mate_at(3),
            SearchScore::mated_at(0),
            SearchScore::mated_at(2),
            SearchScore::from_centipawns(Centipawns::new(300)),
        ]
        .map(Score::from_search);

        assert_eq!(
            reported,
            [
                Score::MateIn(1),
                Score::MateIn(2),
                Score::MateIn(0),
                Score::MateIn(-1),
                Score::Centipawns(Centipawns::new(300)),
            ]
        );
    }
}
