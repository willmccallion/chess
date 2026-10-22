//! Which attacker-target relations a move creates and destroys, for incremental
//! FullThreats updates.
//!
//! The move is replayed as single-piece removals and placements on a lightweight copy of
//! the position. Each step reports the threats of and against the piece, plus the slider
//! rays it opens (removal) or closes (placement).

use crate::eval::PositionView;
use crate::magics;
use crate::types::{Bitboard, Color, Move, Piece, PieceKind, StackVec};

/// One attacker-target relation entering or leaving the position.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct ThreatDelta {
    pub attacker: Piece,
    pub from: u8,
    pub attacked: Piece,
    pub to: u8,
    pub add: bool,
}

/// Bound on deltas per move; overflowing means the accumulator must be refreshed instead.
pub const MAX_THREAT_DELTAS: usize = 160;

#[derive(Clone, Default)]
pub struct ThreatDeltas {
    pub list: StackVec<ThreatDelta, MAX_THREAT_DELTAS>,
    pub overflowed: bool,
}

impl ThreatDeltas {
    pub fn clear(&mut self) {
        self.list.clear();
        self.overflowed = false;
    }

    fn push(&mut self, delta: ThreatDelta) {
        if self.list.len() < MAX_THREAT_DELTAS {
            self.list.push(delta);
        } else {
            self.overflowed = true;
        }
    }
}

const MAX_OVERLAY: usize = 4;

/// The position part-way through a move: piece bitboards plus the squares changed so far.
struct Replay<'b> {
    position: PositionView<'b>,
    piece_bb: [Bitboard; 13],
    occupied: Bitboard,
    overlay: [(u8, Piece); MAX_OVERLAY],
    overlay_len: usize,
}

fn is_slider(piece: Piece) -> bool {
    matches!(
        piece.kind(),
        Some(PieceKind::Bishop | PieceKind::Rook | PieceKind::Queen)
    )
}

fn attacks_of(piece: Piece, sq: usize, occupied: Bitboard) -> Bitboard {
    match piece {
        Piece::WP => magics::WHITE_PAWN_ATTACKS[sq],
        Piece::BP => magics::BLACK_PAWN_ATTACKS[sq],
        Piece::WN | Piece::BN => magics::knight_attacks_from(sq),
        Piece::WB | Piece::BB => magics::get_bishop_attacks(sq, occupied),
        Piece::WR | Piece::BR => magics::get_rook_attacks(sq, occupied),
        Piece::WQ | Piece::BQ => {
            magics::get_bishop_attacks(sq, occupied) | magics::get_rook_attacks(sq, occupied)
        }
        Piece::WK | Piece::BK => magics::king_attacks_from(sq),
        Piece::Empty => 0,
    }
}

impl<'b> Replay<'b> {
    fn new(position: PositionView<'b>) -> Self {
        Self {
            position,
            piece_bb: *position.piece_bb,
            occupied: position.occupied,
            overlay: [(0, Piece::Empty); MAX_OVERLAY],
            overlay_len: 0,
        }
    }

    fn piece_at(&self, sq: usize) -> Piece {
        self.overlay[..self.overlay_len]
            .iter()
            .rev()
            .find(|&&(s, _)| usize::from(s) == sq)
            .map_or(self.position.piece_on[sq], |&(_, piece)| piece)
    }

    fn set(&mut self, sq: usize, piece: Piece) {
        let previous = self.piece_at(sq);
        let bit = 1u64 << sq;
        if !previous.is_empty() {
            self.piece_bb[previous.index()] &= !bit;
        }
        if piece.is_empty() {
            self.occupied &= !bit;
        } else {
            self.piece_bb[piece.index()] |= bit;
            self.occupied |= bit;
        }
        if let Some(entry) = self.overlay[..self.overlay_len]
            .iter_mut()
            .find(|(s, _)| usize::from(*s) == sq)
        {
            entry.1 = piece;
        } else {
            self.overlay[self.overlay_len] = (sq as u8, piece);
            self.overlay_len += 1;
        }
    }

    fn pieces(&self, list: &[Piece]) -> Bitboard {
        list.iter()
            .map(|p| self.piece_bb[p.index()])
            .fold(0, |acc, bb| acc | bb)
    }

    /// Pieces of either color attacking `sq`.
    fn attackers_of(&self, sq: usize) -> Bitboard {
        let occupied = self.occupied;
        (magics::knight_attacks_from(sq) & self.pieces(&[Piece::WN, Piece::BN]))
            | (magics::king_attacks_from(sq) & self.pieces(&[Piece::WK, Piece::BK]))
            | (magics::BLACK_PAWN_ATTACKS[sq] & self.piece_bb[Piece::WP.index()])
            | (magics::WHITE_PAWN_ATTACKS[sq] & self.piece_bb[Piece::BP.index()])
            | (magics::get_rook_attacks(sq, occupied)
                & self.pieces(&[Piece::WR, Piece::BR, Piece::WQ, Piece::BQ]))
            | (magics::get_bishop_attacks(sq, occupied)
                & self.pieces(&[Piece::WB, Piece::BB, Piece::WQ, Piece::BQ]))
    }

    /// The first piece behind `sq` on the line from the slider on `slider_sq`.
    fn piece_behind(&self, slider_sq: usize, sq: usize) -> Option<usize> {
        let line = crate::legality::line_through(slider_sq, sq);
        let from_sq = magics::get_rook_attacks(sq, self.occupied)
            | magics::get_bishop_attacks(sq, self.occupied);
        let behind = line & from_sq & self.occupied & !(1u64 << slider_sq);
        (behind != 0).then(|| behind.trailing_zeros() as usize)
    }

    /// Reports every relation involving the piece on `sq`; `present` says whether the
    /// relations appear (after placing it) or disappear (before removing it).
    fn report_piece(&self, sq: usize, present: bool, out: &mut ThreatDeltas) {
        let piece = self.piece_at(sq);
        let mut targets = attacks_of(piece, sq, self.occupied) & self.occupied;
        while targets != 0 {
            let to = targets.trailing_zeros() as usize;
            targets &= targets - 1;
            out.push(delta(piece, sq, self.piece_at(to), to, present));
        }

        let mut attackers = self.attackers_of(sq);
        while attackers != 0 {
            let from = attackers.trailing_zeros() as usize;
            attackers &= attackers - 1;
            let attacker = self.piece_at(from);
            out.push(delta(attacker, from, piece, sq, present));
            if is_slider(attacker)
                && let Some(behind) = self.piece_behind(from, sq)
            {
                out.push(delta(
                    attacker,
                    from,
                    self.piece_at(behind),
                    behind,
                    !present,
                ));
            }
        }
    }

    fn remove(&mut self, sq: usize, out: &mut ThreatDeltas) {
        self.report_piece(sq, false, out);
        self.set(sq, Piece::Empty);
    }

    fn put(&mut self, sq: usize, piece: Piece, out: &mut ThreatDeltas) {
        self.set(sq, piece);
        self.report_piece(sq, true, out);
    }
}

fn delta(attacker: Piece, from: usize, attacked: Piece, to: usize, add: bool) -> ThreatDelta {
    ThreatDelta {
        attacker,
        from: from as u8,
        attacked,
        to: to as u8,
        add,
    }
}

/// Fills `out` with the threat changes `m` makes; `position` is the position before `m`.
pub fn record_move(position: PositionView, m: Move, out: &mut ThreatDeltas) {
    out.clear();
    let from = m.from as usize;
    let to = m.to as usize;
    let moving = position.piece_on[from];
    let turn = position.turn;
    let mut replay = Replay::new(position);

    replay.remove(from, out);
    if m.capture {
        let captured_sq = match (m.en_passant, turn) {
            (true, Color::White) => to - 8,
            (true, Color::Black) => to + 8,
            (false, _) => to,
        };
        if !replay.piece_at(captured_sq).is_empty() {
            replay.remove(captured_sq, out);
        }
    }
    let placed = m
        .promotion
        .map_or(moving, |kind| Piece::from_kind(kind, turn));
    replay.put(to, placed, out);

    if m.castle {
        let (rook_from, rook_to) = if to > from {
            (to + 1, to - 1)
        } else {
            (to - 2, to + 1)
        };
        let rook = replay.piece_at(rook_from);
        replay.remove(rook_from, out);
        replay.put(rook_to, rook, out);
    }
    cancel_opposites(out);
}

/// Drops relations that the replay both removed and re-added (or the reverse).
fn cancel_opposites(deltas: &mut ThreatDeltas) {
    let list = deltas.list.as_mut_slice();
    let mut len = list.len();
    let mut i = 0;
    while i < len {
        let d = list[i];
        let opposite = (i + 1..len).find(|&j| {
            let o = list[j];
            o.add != d.add
                && o.from == d.from
                && o.to == d.to
                && o.attacker == d.attacker
                && o.attacked == d.attacked
        });
        match opposite {
            Some(j) => {
                list[j] = list[len - 1];
                len -= 1;
                list[i] = list[len - 1];
                len -= 1;
            }
            None => i += 1,
        }
    }
    deltas.list.truncate(len);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;
    use crate::types::MoveList;
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    type Relation = (Piece, u8, Piece, u8);

    fn relations(board: &Board) -> Vec<Relation> {
        let mut out = Vec::new();
        let mut pieces = board.all_pieces;
        while pieces != 0 {
            let from = pieces.trailing_zeros() as usize;
            pieces &= pieces - 1;
            let attacker = board.piece_on[from];
            let mut targets = attacks_of(attacker, from, board.all_pieces) & board.all_pieces;
            while targets != 0 {
                let to = targets.trailing_zeros() as usize;
                targets &= targets - 1;
                out.push((attacker, from as u8, board.piece_on[to], to as u8));
            }
        }
        out.sort_unstable_by_key(|r| (r.0 as u8, r.1, r.2 as u8, r.3));
        out
    }

    fn apply(before: &[Relation], deltas: &ThreatDeltas) -> Vec<Relation> {
        let mut result = before.to_vec();
        for d in deltas.list.as_slice() {
            let relation = (d.attacker, d.from, d.attacked, d.to);
            if d.add {
                result.push(relation);
            } else {
                let position = result
                    .iter()
                    .position(|r| *r == relation)
                    .unwrap_or_else(|| panic!("removed missing relation {relation:?}"));
                result.swap_remove(position);
            }
        }
        result.sort_unstable_by_key(|r| (r.0 as u8, r.1, r.2 as u8, r.3));
        result
    }

    #[test]
    fn deltas_turn_the_old_threats_into_the_new_ones() {
        let starts = [
            crate::types::START_FEN,
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
            "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
        ];
        let mut rng = StdRng::seed_from_u64(7);
        let mut checked = 0;
        let (mut castles, mut en_passants, mut promotions) = (0, 0, 0);
        for fen in starts {
            for _game in 0..40 {
                let mut board = Board::from_fen(fen).unwrap();
                for _ply in 0..120 {
                    let mut moves = MoveList::new();
                    board.generate_legal_moves(&mut moves);
                    if moves.len() == 0 {
                        break;
                    }
                    let m = moves.get(rng.gen_range(0..moves.len()));

                    castles += usize::from(m.castle);
                    en_passants += usize::from(m.en_passant);
                    promotions += usize::from(m.promotion.is_some());
                    let before = relations(&board);
                    let mut deltas = ThreatDeltas::default();
                    record_move(board.position_view(), m, &mut deltas);
                    board.make_move(m);

                    assert!(!deltas.overflowed);
                    assert_eq!(
                        apply(&before, &deltas),
                        relations(&board),
                        "{m:?} -> {}",
                        board.to_fen()
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 10_000);
        assert!(
            castles > 0 && en_passants > 0 && promotions > 0,
            "{castles} {en_passants} {promotions}"
        );
    }
}
