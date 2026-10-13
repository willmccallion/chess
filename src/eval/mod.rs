//! Static evaluation. Each backend implements [`Evaluator`]; a board carries the backend's
//! incremental state in an [`EvalState`] and updates it on every make and unmake.

#[cfg(feature = "stockfish-nnue")]
pub mod stockfish;

use crate::board::Board;
use crate::types::{Bitboard, Color, Move, Piece, StackVec, UpdateBody};

/// Most piece-square changes one move makes: castling moves two pieces.
pub const MAX_PIECE_CHANGES: usize = 5;

/// The parts of a position an evaluator reads, borrowed field by field so a board can
/// update its evaluation state while lending them.
#[derive(Copy, Clone)]
pub struct PositionView<'b> {
    pub piece_on: &'b [Piece; 64],
    pub piece_bb: &'b [Bitboard; 13],
    pub occupied: Bitboard,
    pub turn: Color,
    pub halfmove_clock: i32,
}

/// Piece-square changes of `m` in `position` (before the move), in removal-then-add order.
pub fn record_piece_changes(position: PositionView, m: Move, out: &mut StackVec<UpdateBody, MAX_PIECE_CHANGES>) {
    let from = m.from as usize;
    let to = m.to as usize;
    let moving = position.piece_on[from];
    out.push(UpdateBody { piece: moving, sq: from, add: false });
    if m.capture {
        let cap_sq = match (m.en_passant, position.turn) {
            (true, Color::White) => to - 8,
            (true, Color::Black) => to + 8,
            (false, _) => to,
        };
        let captured = position.piece_on[cap_sq];
        if !captured.is_empty() {
            out.push(UpdateBody { piece: captured, sq: cap_sq, add: false });
        }
    }
    let placed = m.promotion.map_or(moving, |kind| Piece::from_kind(kind, position.turn));
    out.push(UpdateBody { piece: placed, sq: to, add: true });
    if m.castle {
        let (rook_from, rook_to) = if to > from { (to + 1, to - 1) } else { (to - 2, to + 1) };
        let rook = position.piece_on[rook_from];
        out.push(UpdateBody { piece: rook, sq: rook_from, add: false });
        out.push(UpdateBody { piece: rook, sq: rook_to, add: true });
    }
}

/// What a board keeps between moves so an evaluator need not start from scratch.
pub trait IncrementalState {
    /// Records `m`, about to be made in `before`.
    fn push_move(&mut self, before: PositionView, m: Move);
    /// Returns to the position before the last recorded move.
    fn pop_move(&mut self);
    /// Forgets all recorded moves; the current position is evaluated from scratch.
    fn reset(&mut self);
}

/// A static evaluation function.
pub trait Evaluator {
    type State: IncrementalState + Clone;

    fn new_state(&self) -> Self::State;

    /// Centipawns for the side to move in `position`, which `state` describes.
    fn evaluate(&self, state: &mut Self::State, position: PositionView) -> i32;
}

/// Which evaluator to load and from which files, set from UCI options or the command line.
#[derive(Clone, Debug)]
pub struct EvalOptions {
    #[cfg(feature = "stockfish-nnue")]
    stockfish_nets: stockfish::NetPaths,
}

impl EvalOptions {
    /// The networks baked in at build time.
    pub fn built_in() -> Self {
        Self {
            #[cfg(feature = "stockfish-nnue")]
            stockfish_nets: stockfish::NetPaths::built_in_or_empty(),
        }
    }

    /// UCI `option` lines for these settings.
    pub fn uci_options(&self) -> Vec<String> {
        #[cfg_attr(not(feature = "stockfish-nnue"), expect(unused_mut))]
        let mut lines = Vec::new();
        #[cfg(feature = "stockfish-nnue")]
        {
            let nets = &self.stockfish_nets;
            lines.push(format!("option name EvalFile type string default {}", nets.big.display()));
            lines.push(format!("option name EvalFileSmall type string default {}", nets.small.display()));
        }
        lines
    }

    /// Applies `setoption name <name> value <value>`; false if `name` is not one of these.
    pub fn set_uci_option(&mut self, name: &str, value: &str) -> bool {
        #[cfg(feature = "stockfish-nnue")]
        {
            if name.eq_ignore_ascii_case("EvalFile") {
                self.stockfish_nets.big = value.into();
                return true;
            }
            if name.eq_ignore_ascii_case("EvalFileSmall") {
                self.stockfish_nets.small = value.into();
                return true;
            }
        }
        let _ = (name, value);
        false
    }

    /// Loads the evaluator. It cannot be replaced afterwards.
    pub fn load(&self) -> Result<(), String> {
        #[cfg(feature = "stockfish-nnue")]
        return stockfish::ensure_loaded(&self.stockfish_nets);
        #[cfg(not(feature = "stockfish-nnue"))]
        Err("this build has no evaluator".to_string())
    }
}

/// The evaluator in use; chosen once, when its networks load.
#[derive(Clone, Copy)]
enum Active {
    #[cfg(feature = "stockfish-nnue")]
    Stockfish(&'static stockfish::Networks),
}

fn active() -> Option<Active> {
    #[cfg(feature = "stockfish-nnue")]
    if let Some(nets) = stockfish::loaded() {
        return Some(Active::Stockfish(nets));
    }
    None
}

/// The incremental state of whichever evaluator a board was created for.
#[derive(Clone)]
pub enum EvalState {
    /// Created before any evaluator was loaded; replaced on first evaluation.
    Unset,
    #[cfg(feature = "stockfish-nnue")]
    Stockfish(stockfish::accumulator::AccumulatorStack),
}

impl EvalState {
    /// State for the evaluator in use.
    pub fn for_active() -> Self {
        match active() {
            #[cfg(feature = "stockfish-nnue")]
            Some(Active::Stockfish(nets)) => Self::Stockfish(nets.new_state()),
            None => Self::Unset,
        }
    }

    fn matches(&self, active: Active) -> bool {
        match (self, active) {
            #[cfg(feature = "stockfish-nnue")]
            (Self::Stockfish(_), Active::Stockfish(_)) => true,
            #[cfg(feature = "stockfish-nnue")]
            (Self::Unset, _) => false,
        }
    }
}

impl IncrementalState for EvalState {
    #[inline]
    fn push_move(&mut self, before: PositionView, m: Move) {
        match self {
            Self::Unset => {}
            #[cfg(feature = "stockfish-nnue")]
            Self::Stockfish(stack) => stack.push_move(before, m),
        }
    }

    #[inline]
    fn pop_move(&mut self) {
        match self {
            Self::Unset => {}
            #[cfg(feature = "stockfish-nnue")]
            Self::Stockfish(stack) => stack.pop_move(),
        }
    }

    fn reset(&mut self) {
        match self {
            Self::Unset => {}
            #[cfg(feature = "stockfish-nnue")]
            Self::Stockfish(stack) => stack.reset(),
        }
    }
}

/// Static evaluation in centipawns for the side to move; 0 if no evaluator is loaded.
pub fn evaluate(board: &mut Board) -> i32 {
    let Some(active) = active() else {
        return 0;
    };
    let (state, position) = board.eval_state_and_view();
    if !state.matches(active) {
        *state = EvalState::for_active();
    }
    match (active, state) {
        #[cfg(feature = "stockfish-nnue")]
        (Active::Stockfish(nets), EvalState::Stockfish(stack)) => nets.evaluate(stack, position),
        _ => 0,
    }
}
