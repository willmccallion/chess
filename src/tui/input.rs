//! Turning clicks and drags on squares into moves, shared by every screen with a board.

use crate::board::Board;
use crate::types::{Move, PieceKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingPromotion {
    pub from: u8,
    pub to: u8,
}

/// The selected piece, a drag in progress and an open promotion choice.
#[derive(Clone, Debug, Default)]
pub struct MoveInput {
    selected: Option<u8>,
    dragging: bool,
    pending_promotion: Option<PendingPromotion>,
}

impl MoveInput {
    pub fn selected(&self) -> Option<u8> {
        self.selected
    }

    pub fn pending_promotion(&self) -> Option<PendingPromotion> {
        self.pending_promotion
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Squares the selected piece can move to.
    pub fn targets(&self, legal: &[Move]) -> Vec<u8> {
        let Some(from) = self.selected else {
            return Vec::new();
        };
        legal.iter().filter(|m| m.from == from).map(|m| m.to).collect()
    }

    /// A press of the left mouse button on `square`; returns a completed move.
    pub fn press(&mut self, board: &Board, legal: &[Move], square: u8) -> Option<Move> {
        if self.pending_promotion.is_some() {
            return None;
        }
        if let Some(from) = self.selected
            && from != square
            && !moves_between(board, legal, from, square).is_empty()
        {
            return self.move_piece(board, legal, from, square);
        }
        if board.piece_on[usize::from(square)].color() == Some(board.turn) {
            self.selected = Some(square);
            self.dragging = true;
        } else {
            self.selected = None;
        }
        None
    }

    /// A release of the left mouse button on `square`; completes a drag.
    pub fn release(&mut self, board: &Board, legal: &[Move], square: u8) -> Option<Move> {
        let was_dragging = std::mem::take(&mut self.dragging);
        let from = self.selected?;
        if was_dragging && from != square && !moves_between(board, legal, from, square).is_empty() {
            return self.move_piece(board, legal, from, square);
        }
        None
    }

    pub fn release_off_board(&mut self) {
        self.dragging = false;
    }

    fn move_piece(&mut self, board: &Board, legal: &[Move], from: u8, to: u8) -> Option<Move> {
        let candidates = moves_between(board, legal, from, to);
        self.selected = None;
        self.dragging = false;
        match candidates.as_slice() {
            [] => None,
            [only] => Some(*only),
            _ => {
                self.pending_promotion = Some(PendingPromotion { from, to });
                None
            }
        }
    }

    pub fn choose_promotion(&mut self, legal: &[Move], kind: PieceKind) -> Option<Move> {
        let PendingPromotion { from, to } = self.pending_promotion.take()?;
        legal
            .iter()
            .find(|m| m.from == from && m.to == to && m.promotion == Some(kind))
            .copied()
    }
}

/// Legal moves taking the piece on `from` to `to`; several when promoting. Moving the king
/// onto its own rook castles on that side.
fn moves_between(board: &Board, legal: &[Move], from: u8, to: u8) -> Vec<Move> {
    let direct: Vec<Move> = legal.iter().filter(|m| m.from == from && m.to == to).copied().collect();
    if !direct.is_empty() {
        return direct;
    }
    let mover = board.piece_on[usize::from(from)];
    let target = board.piece_on[usize::from(to)];
    let king_onto_own_rook = mover.kind() == Some(PieceKind::King)
        && target.kind() == Some(PieceKind::Rook)
        && target.color() == mover.color();
    if !king_onto_own_rook {
        return Vec::new();
    }
    legal
        .iter()
        .filter(|m| m.castle && m.from == from && (m.to > m.from) == (to > from))
        .copied()
        .collect()
}
