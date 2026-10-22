//! Endgame drills: positions to win or hold against the engine's best defence.

use crate::board::Board;
use crate::types::{Color, Piece, PieceKind};

/// What you have to do in a drill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Goal {
    Checkmate,
    /// Make a queen that survives, with the opponent left with only a king.
    Promote,
    /// Hold a draw for this many of your moves (or reach a drawn result sooner).
    Hold {
        moves: usize,
    },
}

impl Goal {
    pub fn describe(self) -> String {
        match self {
            Self::Checkmate => "Checkmate the lone king.".to_string(),
            Self::Promote => "Promote your pawn and keep the new queen.".to_string(),
            Self::Hold { moves } => format!("Hold the draw for {moves} moves."),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drill {
    pub id: &'static str,
    pub name: &'static str,
    pub fen: &'static str,
    pub goal: Goal,
    /// Your moves a strong player needs; finishing within it earns full marks.
    pub target_moves: Option<usize>,
    pub lesson: &'static str,
    pub tips: &'static [&'static str],
}

impl Drill {
    pub fn board(&self) -> Result<Board, String> {
        Board::from_fen(self.fen)
    }

    pub fn side(&self) -> Result<Color, String> {
        self.board().map(|b| b.turn)
    }
}

pub const DRILLS: &[Drill] = &[
    Drill {
        id: "two-rooks",
        name: "Two rooks: the ladder",
        fen: "8/8/3k4/8/8/8/8/R3K2R w - - 0 1",
        goal: Goal::Checkmate,
        target_moves: Some(8),
        lesson: "The rooks take turns cutting the king off one rank at a time, like climbing a ladder, \
                 until it is pushed to the edge.",
        tips: &[
            "One rook checks, the other guards the next rank so the king cannot come back.",
            "If the king attacks a rook, move that rook far away along its rank.",
            "Your own king is not needed at all.",
        ],
    },
    Drill {
        id: "queen-mate",
        name: "Queen and king against king",
        fen: "8/8/8/4k3/8/8/8/3QK3 w - - 0 1",
        goal: Goal::Checkmate,
        target_moves: Some(10),
        lesson: "Use the queen to shrink the king's box, a knight's move away from it, then bring your \
                 own king up to help deliver mate.",
        tips: &[
            "Keep the queen a knight's move from the enemy king and follow it as the box shrinks.",
            "When the king is on the edge, stop: bring your king up before you check.",
            "Always check the opponent has a legal move, or it is stalemate.",
        ],
    },
    Drill {
        id: "rook-mate",
        name: "Rook and king against king",
        fen: "8/8/8/4k3/8/8/8/R3K3 w - - 0 1",
        goal: Goal::Checkmate,
        target_moves: Some(16),
        lesson: "The rook cuts the king off and your king does the pushing: face the enemy king \
                 (opposition), then check to drive it back a rank.",
        tips: &[
            "Use the rook to make a box, and keep making the box smaller.",
            "When the kings face each other with one square between, check with the rook.",
            "If the rook is attacked, move it to the far side of the board on the same line.",
        ],
    },
    Drill {
        id: "kp-key-squares",
        name: "King and pawn: key squares",
        fen: "4k3/8/8/8/8/8/4P3/4K3 w - - 0 1",
        goal: Goal::Promote,
        target_moves: Some(12),
        lesson: "A pawn queens if your king reaches a key square: for a pawn on its second to fourth \
                 rank, those are the three squares two ranks in front of it. King first, pawn later.",
        tips: &[
            "Walk your king in front of the pawn before pushing it.",
            "Kings face each other with one square between: that is the opposition, and the side \
             not to move must give way.",
            "Push the pawn only when your king already controls the square in front of it.",
        ],
    },
    Drill {
        id: "kp-defend",
        name: "Defending king against king and pawn",
        fen: "8/4k3/8/8/4P3/4K3/8/8 b - - 0 1",
        goal: Goal::Hold { moves: 20 },
        target_moves: None,
        lesson: "Stay in front of the pawn and take the opposition: face the enemy king with one \
                 square between when it is their move.",
        tips: &[
            "Keep your king on the pawn's file, in front of it.",
            "When the pawn reaches your second rank, go straight back in front of it, not sideways.",
            "If you are pushed to the last rank, step back so you can answer with the opposition.",
        ],
    },
    Drill {
        id: "lucena",
        name: "Lucena: build a bridge",
        fen: "1K1k4/1P6/8/8/8/8/r7/2R5 w - - 0 1",
        goal: Goal::Promote,
        target_moves: Some(10),
        lesson: "The most important rook ending to win. Cut the enemy king off, bring your rook to \
                 the fourth rank, then walk your king out and block checks with the rook: the bridge.",
        tips: &[
            "First check to push the black king further away (Rd1+).",
            "Then Rd4 (or Rc4): the rook on the fourth rank will block checks later.",
            "Walk your king out towards the rook; when checked on the file, block with the rook.",
        ],
    },
    Drill {
        id: "philidor",
        name: "Philidor: the third-rank defence",
        fen: "4k3/R7/8/4PK2/8/8/8/1r6 b - - 0 1",
        goal: Goal::Hold { moves: 25 },
        target_moves: None,
        lesson: "Rook on your third rank keeps the enemy king out. When the pawn advances to that rank, \
                 swing the rook to the back of the board and check from behind.",
        tips: &[
            "Rook to your third rank now (here ...Rb6).",
            "Keep it there until the pawn steps onto that rank.",
            "Then go to the first rank and check from far away: the king has nowhere to hide.",
        ],
    },
];

pub fn drill_by_id(id: &str) -> Option<&'static Drill> {
    DRILLS.iter().find(|d| d.id == id)
}

/// Whether `side` has a queen while the other side has only its king.
pub fn has_won_queen_ending(board: &Board, side: Color) -> bool {
    let queen = board.piece_bb[Piece::from_kind(PieceKind::Queen, side).index()] != 0;
    let other_pieces = match side.other() {
        Color::White => board.w_pieces,
        Color::Black => board.b_pieces,
    };
    queen && other_pieces.count_ones() == 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_drill_position_reads() {
        for drill in DRILLS {
            assert!(drill.board().is_ok(), "{}", drill.id);
        }
    }

    #[test]
    fn drill_ids_are_unique() {
        let ids: HashSet<&str> = DRILLS.iter().map(|d| d.id).collect();

        assert_eq!(ids.len(), DRILLS.len());
    }

    #[test]
    fn a_queen_against_a_bare_king_is_won() {
        let won = Board::from_fen("4k3/8/8/8/8/8/8/3QK3 w - - 0 1").unwrap();
        let both_queens = Board::from_fen("3qk3/8/8/8/8/8/8/3QK3 w - - 0 1").unwrap();

        assert!(has_won_queen_ending(&won, Color::White));
        assert!(!has_won_queen_ending(&both_queens, Color::White));
    }
}
