//! Accumulators for every position on the current line, updated lazily from the piece
//! changes each move records.

use super::network::{Column, Network};
use crate::eval::{Change, MAX_PIECE_CHANGES, PieceChange, PositionView, record_piece_changes};
use crate::types::{Color, Move, Piece, PieceKind, StackVec};

/// Bullet's `Chess768` index of `owner`'s `kind` on `square`, seen from `perspective`:
/// the perspective's own pieces first, ranks flipped for Black.
pub(super) const fn feature_index(
    perspective: Color,
    owner: Color,
    kind: PieceKind,
    square: usize,
) -> usize {
    let side = if owner as usize == perspective as usize {
        0
    } else {
        384
    };
    let relative_square = match perspective {
        Color::White => square,
        Color::Black => square ^ 0b11_1000,
    };
    side + 64 * kind as usize + relative_square
}

fn piece_feature(perspective: Color, piece: Piece, square: usize) -> Option<usize> {
    Some(feature_index(
        perspective,
        piece.color()?,
        piece.kind()?,
        square,
    ))
}

/// Features active in `position` from `perspective`.
pub fn active_features(position: PositionView, perspective: Color) -> impl Iterator<Item = usize> {
    (0..64).filter_map(move |square| piece_feature(perspective, position.piece_on[square], square))
}

/// Accumulators of one position, indexed by perspective.
type Pair = [Column; 2];

fn refreshed(net: &Network, position: PositionView) -> Pair {
    [Color::White, Color::Black].map(|perspective| {
        let mut column = net.feature_bias;
        for feature in active_features(position, perspective) {
            column.add(net.feature(feature));
        }
        column
    })
}

fn updated(net: &Network, parent: &Pair, changes: &[PieceChange]) -> Pair {
    let mut pair = *parent;
    for perspective in [Color::White, Color::Black] {
        let column = &mut pair[perspective as usize];
        for change in changes {
            let Some(feature) = piece_feature(perspective, change.piece, change.sq) else {
                continue;
            };
            match change.action {
                Change::Add => column.add(net.feature(feature)),
                Change::Remove => column.sub(net.feature(feature)),
            }
        }
    }
    pair
}

#[derive(Clone)]
struct Ply {
    /// What the move into this position changed.
    changes: StackVec<PieceChange, MAX_PIECE_CHANGES>,
    accumulators: Pair,
    computed: bool,
}

impl Ply {
    fn new() -> Self {
        Self {
            changes: StackVec::default(),
            accumulators: [Column::ZERO; 2],
            computed: false,
        }
    }
}

pub struct AccumulatorStack {
    plies: Vec<Ply>,
    top: usize,
}

impl Clone for AccumulatorStack {
    fn clone(&self) -> Self {
        Self {
            plies: self.plies[..=self.top].to_vec(),
            top: self.top,
        }
    }
}

impl Default for AccumulatorStack {
    fn default() -> Self {
        Self::new()
    }
}

impl AccumulatorStack {
    pub fn new() -> Self {
        Self {
            plies: vec![Ply::new()],
            top: 0,
        }
    }

    /// Forgets the line: the current position becomes an uncomputed root.
    pub fn reset(&mut self) {
        self.top = 0;
        self.plies[0].computed = false;
    }

    /// Records `m`, about to be made in `before`.
    pub fn push_move(&mut self, before: PositionView, m: Move) {
        self.top += 1;
        if self.plies.len() == self.top {
            self.plies.push(Ply::new());
        }
        let ply = &mut self.plies[self.top];
        ply.computed = false;
        ply.changes.clear();
        record_piece_changes(before, m, &mut ply.changes);
    }

    /// Steps back a move. Past the first ply the stack recorded, as on a
    /// board whose state was made mid-line, the position is one it never
    /// held, so it is forgotten and refreshed when next evaluated.
    pub fn pop_move(&mut self) {
        if self.top == 0 {
            self.reset();
        } else {
            self.top -= 1;
        }
    }

    /// Accumulators of the current position, which is `position`.
    pub fn current(&mut self, net: &Network, position: PositionView) -> &Pair {
        if let Some(base) = self.latest_computed() {
            self.update_from(net, base);
        } else {
            let ply = &mut self.plies[self.top];
            ply.accumulators = refreshed(net, position);
            ply.computed = true;
        }
        &self.plies[self.top].accumulators
    }

    fn latest_computed(&self) -> Option<usize> {
        (0..=self.top)
            .rev()
            .find(|&index| self.plies[index].computed)
    }

    fn update_from(&mut self, net: &Network, base: usize) {
        for index in base + 1..=self.top {
            let (before, after) = self.plies.split_at_mut(index);
            let ply = &mut after[0];
            ply.accumulators =
                updated(net, &before[index - 1].accumulators, ply.changes.as_slice());
            ply.computed = true;
        }
    }
}
