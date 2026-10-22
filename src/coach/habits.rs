//! Opening habits that cost beginners games: checked against the basic principles of development.

use crate::board::Board;
use crate::types::{Color, Move, Piece, PieceKind, START_FEN};

const EARLY_QUEEN_MOVES: usize = 6;
const REPEAT_PIECE_MOVES: usize = 6;
const KING_WALK_MOVES: usize = 10;
const DEVELOPED_BY_MOVE: usize = 10;
const CASTLED_BY_MOVE: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Habit {
    EarlyQueen,
    SamePieceTwice,
    KingWalk,
    SlowDevelopment,
    NoCastling,
}

impl Habit {
    pub const ALL: [Self; 5] = [
        Self::EarlyQueen,
        Self::SamePieceTwice,
        Self::KingWalk,
        Self::SlowDevelopment,
        Self::NoCastling,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::EarlyQueen => "Early queen moves",
            Self::SamePieceTwice => "Moving a piece twice",
            Self::KingWalk => "King moves in the opening",
            Self::SlowDevelopment => "Slow development",
            Self::NoCastling => "Late castling",
        }
    }

    pub const fn advice(self) -> &'static str {
        match self {
            Self::EarlyQueen => {
                "The queen came out while your knights and bishops were still at home. \
                 It gets chased around and you lose time: develop the minor pieces first."
            }
            Self::SamePieceTwice => {
                "A piece moved twice before the others had moved once. Unless it wins something \
                 or escapes an attack, bring a new piece out instead."
            }
            Self::KingWalk => {
                "Your king moved in the opening and lost the right to castle. Keep it home and \
                 castle; a king in the middle gets attacked."
            }
            Self::SlowDevelopment => {
                "By move 10, two or more of your knights and bishops had not moved. Get every \
                 minor piece out in the first ten moves."
            }
            Self::NoCastling => {
                "Your king was still in the centre after move 12. Castle early: it keeps the \
                 king safe and brings a rook into the game."
            }
        }
    }
}

/// A habit seen in a game, at the half-move where it showed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HabitNote {
    pub habit: Habit,
    pub ply: usize,
}

/// The habits `side` showed in a game from the standard starting position.
pub fn opening_habits(start: &Board, moves: &[Move], side: Color) -> Vec<HabitNote> {
    if start.to_fen() != START_FEN {
        return Vec::new();
    }
    let mut notes: Vec<HabitNote> = Vec::new();
    let mut note = |habit: Habit, ply: usize| {
        if !notes.iter().any(|n| n.habit == habit) {
            notes.push(HabitNote { habit, ply });
        }
    };
    let mut board = start.without_eval_state();
    let mut moved_from_home: Vec<u8> = Vec::new();
    let mut own_moves = 0;
    let mut castled = false;
    for (ply, &m) in moves.iter().enumerate() {
        if board.turn != side {
            board.make_move(m);
            continue;
        }
        own_moves += 1;
        let kind = board.piece_on[usize::from(m.from)].kind();
        let undeveloped = undeveloped_minors(&board, side);
        if kind == Some(PieceKind::Queen)
            && !m.capture
            && own_moves <= EARLY_QUEEN_MOVES
            && undeveloped >= 2
        {
            note(Habit::EarlyQueen, ply);
        }
        let is_minor = matches!(kind, Some(PieceKind::Knight | PieceKind::Bishop));
        let moved_before = moved_from_home.contains(&m.from);
        if is_minor
            && moved_before
            && !m.capture
            && own_moves <= REPEAT_PIECE_MOVES
            && undeveloped >= 1
            && !board.is_square_attacked(i32::from(m.from), side.other())
        {
            note(Habit::SamePieceTwice, ply);
        }
        if kind == Some(PieceKind::King)
            && !m.castle
            && own_moves <= KING_WALK_MOVES
            && queens_on(&board)
        {
            note(Habit::KingWalk, ply);
        }
        castled |= m.castle;
        moved_from_home.retain(|&square| square != m.from);
        moved_from_home.push(m.to);
        board.make_move(m);
        if own_moves == DEVELOPED_BY_MOVE && undeveloped_minors(&board, side) >= 2 {
            note(Habit::SlowDevelopment, ply);
        }
        if own_moves == CASTLED_BY_MOVE && !castled && king_on_home_square(&board, side) {
            note(Habit::NoCastling, ply);
        }
    }
    notes
}

fn undeveloped_minors(board: &Board, side: Color) -> usize {
    let (rank, knight, bishop) = match side {
        Color::White => (0, Piece::WN, Piece::WB),
        Color::Black => (7, Piece::BN, Piece::BB),
    };
    let home = |file: u8, piece: Piece| board.piece_on[usize::from(rank * 8 + file)] == piece;
    [(1, knight), (6, knight), (2, bishop), (5, bishop)]
        .into_iter()
        .filter(|&(file, piece)| home(file, piece))
        .count()
}

fn queens_on(board: &Board) -> bool {
    board.piece_bb[Piece::WQ.index()] | board.piece_bb[Piece::BQ.index()] != 0
}

fn king_on_home_square(board: &Board, side: Color) -> bool {
    let home = if side == Color::White { 4 } else { 60 };
    board.king_square(side) == home
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notation::parse_san_line;

    fn habits(sans: &[&str], side: Color) -> Vec<Habit> {
        let start = Board::from_fen(START_FEN).unwrap();
        let moves = parse_san_line(&start, sans).unwrap();
        opening_habits(&start, &moves, side)
            .into_iter()
            .map(|n| n.habit)
            .collect()
    }

    #[test]
    fn bringing_the_queen_out_on_move_two_is_an_early_queen() {
        assert_eq!(
            habits(&["e4", "e5", "Qh5", "Nc6"], Color::White),
            vec![Habit::EarlyQueen]
        );
    }

    #[test]
    fn recapturing_with_the_queen_is_not_an_early_queen() {
        assert!(habits(&["e4", "d5", "exd5", "Qxd5"], Color::Black).is_empty());
    }

    #[test]
    fn moving_a_knight_again_for_no_reason_is_noted() {
        assert_eq!(
            habits(&["Nf3", "e5", "Ng5", "d5"], Color::White),
            vec![Habit::SamePieceTwice]
        );
    }

    #[test]
    fn a_knight_escaping_a_pawn_attack_may_move_again() {
        assert!(habits(&["Nf3", "e5", "e3", "e4", "Nd4"], Color::White).is_empty());
    }

    #[test]
    fn walking_the_king_with_queens_on_is_noted() {
        assert_eq!(
            habits(&["e4", "e5", "Ke2"], Color::White),
            vec![Habit::KingWalk]
        );
    }

    #[test]
    fn a_good_opening_shows_no_bad_habits() {
        let italian = [
            "e4", "e5", "Nf3", "Nc6", "Bc4", "Bc5", "c3", "Nf6", "d3", "d6", "O-O", "O-O", "Re1",
            "a6", "Bb3", "Ba7", "Nbd2", "h6", "Nf1", "Re8", "Ng3", "Be6", "h3", "Qd7",
        ];

        assert!(habits(&italian, Color::White).is_empty());
        assert!(habits(&italian, Color::Black).is_empty());
    }

    #[test]
    fn shuffling_pawns_for_ten_moves_is_slow_development_and_late_castling() {
        let pawns = [
            "a3", "a6", "b3", "b6", "c3", "c6", "d3", "d6", "e3", "e6", "f3", "f6", "g3", "g6",
            "h3", "h6", "a4", "a5", "b4", "b5", "c4", "c5", "d4", "d5",
        ];

        assert_eq!(
            habits(&pawns, Color::White),
            vec![Habit::SlowDevelopment, Habit::NoCastling]
        );
    }
}
