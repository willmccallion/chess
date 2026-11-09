//! Static evaluation. Each backend implements [`Evaluator`]; a board carries the backend's
//! incremental state in an [`EvalState`] and updates it on every make and unmake.

pub mod bootstrap;
pub mod own;
#[cfg(feature = "stockfish-nnue")]
pub mod stockfish;

use crate::board::Board;
use crate::types::{Bitboard, Color, Move, Piece, StackVec, UpdateBody, castling_rook_squares};
use bootstrap::{Bootstrap, NoState};
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{PoisonError, RwLock};

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
pub fn record_piece_changes(
    position: PositionView,
    m: Move,
    out: &mut StackVec<UpdateBody, MAX_PIECE_CHANGES>,
) {
    let from = m.from as usize;
    let to = m.to as usize;
    let moving = position.piece_on[from];
    out.push(UpdateBody {
        piece: moving,
        sq: from,
        add: false,
    });
    if m.capture {
        let cap_sq = m.captured_square(position.turn);
        let captured = position.piece_on[cap_sq];
        if !captured.is_empty() {
            out.push(UpdateBody {
                piece: captured,
                sq: cap_sq,
                add: false,
            });
        }
    }
    let placed = m
        .promotion
        .map_or(moving, |kind| Piece::from_kind(kind, position.turn));
    out.push(UpdateBody {
        piece: placed,
        sq: to,
        add: true,
    });
    if m.castle {
        let (rook_from, rook_to) = castling_rook_squares(from, to);
        let rook = position.piece_on[rook_from];
        out.push(UpdateBody {
            piece: rook,
            sq: rook_from,
            add: false,
        });
        out.push(UpdateBody {
            piece: rook,
            sq: rook_to,
            add: true,
        });
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
    /// Our own network, trained on self-play games.
    Own,
}

impl EvalKind {
    pub const ALL: &[Self] = &[
        #[cfg(feature = "stockfish-nnue")]
        Self::Stockfish,
        Self::Bootstrap,
        Self::Own,
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
            Self::Own => "own",
        }
    }
}

impl std::str::FromStr for EvalKind {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, String> {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.name().eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                let names: Vec<&str> = Self::ALL.iter().map(|kind| kind.name()).collect();
                format!(
                    "unknown evaluator '{name}'; expected one of: {}",
                    names.join(", ")
                )
            })
    }
}

/// Which evaluator to load and from which files, set from UCI options or the command line.
#[derive(Clone, Debug)]
pub struct EvalOptions {
    pub kind: EvalKind,
    #[cfg(feature = "stockfish-nnue")]
    stockfish_nets: stockfish::NetPaths,
    own_net: PathBuf,
}

impl EvalOptions {
    /// The default evaluator, with the networks baked in at build time.
    pub fn built_in() -> Self {
        Self {
            kind: EvalKind::DEFAULT,
            #[cfg(feature = "stockfish-nnue")]
            stockfish_nets: stockfish::NetPaths::built_in_or_empty(),
            own_net: own::default_net_path(),
        }
    }

    pub fn with_kind(kind: EvalKind) -> Self {
        Self {
            kind,
            ..Self::built_in()
        }
    }

    /// Uses `path` for the own network instead of the one baked in at build time.
    #[must_use]
    pub fn with_own_net(self, path: PathBuf) -> Self {
        Self {
            own_net: path,
            ..self
        }
    }

    /// UCI `option` lines for these settings.
    pub fn uci_options(&self) -> Vec<String> {
        let kinds: String = EvalKind::ALL
            .iter()
            .map(|kind| format!(" var {}", kind.name()))
            .collect();
        #[cfg_attr(not(feature = "stockfish-nnue"), expect(unused_mut))]
        let mut lines = vec![
            format!(
                "option name Evaluator type combo default {}{kinds}",
                self.kind.name()
            ),
            format!(
                "option name OwnEvalFile type string default {}",
                self.own_net.display()
            ),
        ];
        #[cfg(feature = "stockfish-nnue")]
        {
            let nets = &self.stockfish_nets;
            lines.push(format!(
                "option name EvalFile type string default {}",
                nets.big.display()
            ));
            lines.push(format!(
                "option name EvalFileSmall type string default {}",
                nets.small.display()
            ));
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
        if name.eq_ignore_ascii_case("OwnEvalFile") {
            self.own_net = value.into();
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

    /// Makes these settings' evaluator the one in use, reading its networks
    /// unless this process already has. Returns whether it replaced another;
    /// on an error the evaluator in use stays.
    pub fn load(&self) -> Result<bool, String> {
        let wanted = match self.kind {
            #[cfg(feature = "stockfish-nnue")]
            EvalKind::Stockfish => Active::Stockfish(stockfish::load(&self.stockfish_nets)?),
            EvalKind::Bootstrap => Active::Bootstrap,
            EvalKind::Own => Active::Own(own::load(&self.own_net)?),
        };
        let replaced = active().is_none_or(|current| !current.is_same(wanted));
        if replaced {
            set_active(wanted);
        }
        Ok(replaced)
    }
}

/// Whether an evaluator has been loaded.
pub fn is_loaded() -> bool {
    active().is_some()
}

/// The evaluator in use.
#[derive(Clone, Copy)]
enum Active {
    #[cfg(feature = "stockfish-nnue")]
    Stockfish(&'static stockfish::Networks),
    Bootstrap,
    Own(&'static own::Network),
}

impl Active {
    /// Whether `self` and `other` evaluate with the same networks.
    fn is_same(self, other: Self) -> bool {
        match (self, other) {
            #[cfg(feature = "stockfish-nnue")]
            (Self::Stockfish(a), Self::Stockfish(b)) => std::ptr::eq(a, b),
            (Self::Bootstrap, Self::Bootstrap) => true,
            (Self::Own(a), Self::Own(b)) => std::ptr::eq(a, b),
            _ => false,
        }
    }
}

static ACTIVE: RwLock<Option<Active>> = RwLock::new(None);
/// Bumped each time the evaluator is replaced, so a thread re-reads
/// `ACTIVE` only then.
static ACTIVE_GENERATION: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The evaluator this thread last read, and the generation it read it in.
    static SEEN: Cell<(u64, Option<Active>)> = const { Cell::new((u64::MAX, None)) };
}

/// The evaluator in use. A thread that has not yet seen a replacement keeps
/// the one before it, which stays valid: networks are never freed.
fn active() -> Option<Active> {
    let generation = ACTIVE_GENERATION.load(Ordering::Acquire);
    SEEN.with(|seen| {
        let (seen_generation, active) = seen.get();
        if seen_generation == generation {
            return active;
        }
        let active = *ACTIVE.read().unwrap_or_else(PoisonError::into_inner);
        seen.set((generation, active));
        active
    })
}

fn set_active(active: Active) {
    *ACTIVE.write().unwrap_or_else(PoisonError::into_inner) = Some(active);
    ACTIVE_GENERATION.fetch_add(1, Ordering::Release);
}

/// The incremental state of whichever evaluator a board was created for.
#[derive(Clone)]
pub enum EvalState {
    /// Nothing kept: the evaluator needs no state, or none was loaded when the board was made.
    Stateless,
    #[cfg(feature = "stockfish-nnue")]
    Stockfish(stockfish::accumulator::AccumulatorStack),
    Own(own::AccumulatorStack),
}

impl EvalState {
    /// State for the evaluator in use.
    pub fn for_active() -> Self {
        match active() {
            #[cfg(feature = "stockfish-nnue")]
            Some(Active::Stockfish(nets)) => Self::Stockfish(nets.new_state()),
            Some(Active::Own(net)) => Self::Own(net.new_state()),
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
            Self::Own(stack) => stack.push_move(before, m),
        }
    }

    #[inline]
    fn pop_move(&mut self) {
        match self {
            Self::Stateless => {}
            #[cfg(feature = "stockfish-nnue")]
            Self::Stockfish(stack) => stack.pop_move(),
            Self::Own(stack) => stack.pop_move(),
        }
    }

    fn reset(&mut self) {
        match self {
            Self::Stateless => {}
            #[cfg(feature = "stockfish-nnue")]
            Self::Stockfish(stack) => stack.reset(),
            Self::Own(stack) => stack.reset(),
        }
    }
}

/// Static evaluation in centipawns for the side to move; 0 if no evaluator is loaded.
pub fn evaluate(board: &mut Board) -> i32 {
    let Some(active) = active() else {
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
        Active::Own(net) => {
            let (state, position) = board.eval_state_and_view();
            if let EvalState::Own(stack) = state {
                return net.evaluate(stack, position);
            }
            let mut stack = net.new_state();
            let score = net.evaluate(&mut stack, position);
            *state = EvalState::Own(stack);
            score
        }
    }
}
