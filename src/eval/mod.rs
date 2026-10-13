//! Static evaluation. Each backend implements [`Evaluator`]; a board carries the backend's
//! incremental state in an [`EvalState`] and updates it on every make and unmake.

pub mod bootstrap;
#[cfg(feature = "stockfish-nnue")]
pub mod stockfish;

use crate::board::Board;
use crate::types::{Bitboard, Color, Move, Piece, StackVec, UpdateBody};
use bootstrap::{Bootstrap, NoState};
use std::sync::OnceLock;

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

/// The evaluators this build can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvalKind {
    /// Stockfish 18's networks.
    #[cfg(feature = "stockfish-nnue")]
    Stockfish,
    /// The hand-written evaluation that plays the first training games.
    Bootstrap,
}

impl EvalKind {
    pub const ALL: &[Self] = &[
        #[cfg(feature = "stockfish-nnue")]
        Self::Stockfish,
        Self::Bootstrap,
    ];

    #[cfg(feature = "stockfish-nnue")]
    pub const DEFAULT: Self = Self::Stockfish;
    #[cfg(not(feature = "stockfish-nnue"))]
    pub const DEFAULT: Self = Self::Bootstrap;

    pub const fn name(self) -> &'static str {
        match self {
            #[cfg(feature = "stockfish-nnue")]
            Self::Stockfish => "stockfish",
            Self::Bootstrap => "bootstrap",
        }
    }
}

impl std::str::FromStr for EvalKind {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, String> {
        Self::ALL.iter().copied().find(|kind| kind.name().eq_ignore_ascii_case(name)).ok_or_else(|| {
            let names: Vec<&str> = Self::ALL.iter().map(|kind| kind.name()).collect();
            format!("unknown evaluator '{name}'; expected one of: {}", names.join(", "))
        })
    }
}

/// Which evaluator to load and from which files, set from UCI options or the command line.
#[derive(Clone, Debug)]
pub struct EvalOptions {
    pub kind: EvalKind,
    #[cfg(feature = "stockfish-nnue")]
    stockfish_nets: stockfish::NetPaths,
}

impl EvalOptions {
    /// The default evaluator, with the networks baked in at build time.
    pub fn built_in() -> Self {
        Self {
            kind: EvalKind::DEFAULT,
            #[cfg(feature = "stockfish-nnue")]
            stockfish_nets: stockfish::NetPaths::built_in_or_empty(),
        }
    }

    pub fn with_kind(kind: EvalKind) -> Self {
        Self { kind, ..Self::built_in() }
    }

    /// UCI `option` lines for these settings.
    pub fn uci_options(&self) -> Vec<String> {
        let kinds: String = EvalKind::ALL.iter().map(|kind| format!(" var {}", kind.name())).collect();
        #[cfg_attr(not(feature = "stockfish-nnue"), expect(unused_mut))]
        let mut lines = vec![format!("option name Evaluator type combo default {}{kinds}", self.kind.name())];
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
        if name.eq_ignore_ascii_case("Evaluator") {
            if let Ok(kind) = value.parse() {
                self.kind = kind;
            }
            return true;
        }
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
        false
    }

    /// Loads the evaluator. It cannot be replaced afterwards.
    pub fn load(&self) -> Result<(), String> {
        if let Some(loaded) = ACTIVE.get() {
            return if loaded.kind() == self.kind {
                Ok(())
            } else {
                Err(format!("the {} evaluator is already loaded", loaded.kind().name()))
            };
        }
        let active = match self.kind {
            #[cfg(feature = "stockfish-nnue")]
            EvalKind::Stockfish => {
                stockfish::ensure_loaded(&self.stockfish_nets)?;
                Active::Stockfish(stockfish::loaded().ok_or("the Stockfish networks did not load")?)
            }
            EvalKind::Bootstrap => Active::Bootstrap,
        };
        let _ = ACTIVE.set(active);
        Ok(())
    }
}

/// The evaluator in use.
#[derive(Clone, Copy)]
enum Active {
    #[cfg(feature = "stockfish-nnue")]
    Stockfish(&'static stockfish::Networks),
    Bootstrap,
}

impl Active {
    const fn kind(self) -> EvalKind {
        match self {
            #[cfg(feature = "stockfish-nnue")]
            Self::Stockfish(_) => EvalKind::Stockfish,
            Self::Bootstrap => EvalKind::Bootstrap,
        }
    }
}

static ACTIVE: OnceLock<Active> = OnceLock::new();

/// The incremental state of whichever evaluator a board was created for.
#[derive(Clone)]
pub enum EvalState {
    /// Nothing kept: the evaluator needs no state, or none was loaded when the board was made.
    Stateless,
    #[cfg(feature = "stockfish-nnue")]
    Stockfish(stockfish::accumulator::AccumulatorStack),
}

impl EvalState {
    /// State for the evaluator in use.
    pub fn for_active() -> Self {
        match ACTIVE.get() {
            #[cfg(feature = "stockfish-nnue")]
            Some(Active::Stockfish(nets)) => Self::Stockfish(nets.new_state()),
            Some(Active::Bootstrap) | None => Self::Stateless,
        }
    }
}

impl IncrementalState for EvalState {
    #[inline]
    fn push_move(&mut self, before: PositionView, m: Move) {
        match self {
            Self::Stateless => NoState.push_move(before, m),
            #[cfg(feature = "stockfish-nnue")]
            Self::Stockfish(stack) => stack.push_move(before, m),
        }
    }

    #[inline]
    fn pop_move(&mut self) {
        match self {
            Self::Stateless => {}
            #[cfg(feature = "stockfish-nnue")]
            Self::Stockfish(stack) => stack.pop_move(),
        }
    }

    fn reset(&mut self) {
        match self {
            Self::Stateless => {}
            #[cfg(feature = "stockfish-nnue")]
            Self::Stockfish(stack) => stack.reset(),
        }
    }
}

/// Static evaluation in centipawns for the side to move; 0 if no evaluator is loaded.
pub fn evaluate(board: &mut Board) -> i32 {
    let Some(&active) = ACTIVE.get() else {
        return 0;
    };
    match active {
        Active::Bootstrap => Bootstrap.evaluate(&mut NoState, board.position_view()),
        #[cfg(feature = "stockfish-nnue")]
        Active::Stockfish(nets) => {
            let (state, position) = board.eval_state_and_view();
            if let EvalState::Stockfish(stack) = state {
                return nets.evaluate(stack, position);
            }
            let mut stack = nets.new_state();
            let score = nets.evaluate(&mut stack, position);
            *state = EvalState::Stockfish(stack);
            score
        }
    }
}
