//! Engine scores as winning chances, and moves judged by how much winning chance they gave away.

use crate::board::Board;
use crate::search::Score;
use crate::types::{Color, PieceKind};

/// Lichess's fit of expected score to centipawns.
const WIN_CHANCE_SLOPE: f64 = 0.003_682_08;
/// Scores beyond this many centipawns are all but decided; clamping keeps them comparable.
const CP_CLAMP: i32 = 1000;

/// Win-chance drops (in percentage points) at which a move counts as each kind of error.
const INACCURACY_DROP: f64 = 5.0;
const MISTAKE_DROP: f64 = 10.0;
const BLUNDER_DROP: f64 = 15.0;
const NEAR_BEST_DROP: f64 = 1.0;

/// Positions with at most this much non-pawn material (both sides, 3/3/5/9) are endgames.
const ENDGAME_MATERIAL: i32 = 26;
/// Plies after which the opening is over.
const OPENING_PLIES: usize = 20;

/// An engine verdict on a position, for the side to move there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eval {
    Score(Score),
    /// The side to move has been checkmated.
    Checkmated,
}

impl Eval {
    pub const DRAWN: Self = Self::Score(Score::Centipawns(0));

    /// The chance the side to move wins (draws counting half).
    pub fn win_chance(self) -> WinChance {
        match self {
            Self::Checkmated => WinChance::LOST,
            Self::Score(Score::MateIn(moves)) if moves > 0 => WinChance::WON,
            Self::Score(Score::MateIn(_)) => WinChance::LOST,
            Self::Score(Score::Centipawns(cp)) => {
                let cp = f64::from(cp.clamp(-CP_CLAMP, CP_CLAMP));
                WinChance(100.0 / (1.0 + (-WIN_CHANCE_SLOPE * cp).exp()))
            }
        }
    }

    /// The chance `side` wins when `side_to_move` is to move.
    pub fn win_chance_for(self, side_to_move: Color, side: Color) -> WinChance {
        let chance = self.win_chance();
        if side == side_to_move { chance } else { chance.for_opponent() }
    }

    /// Moves until the side to move mates (positive) or is mated (negative), if forced.
    pub const fn mate_in(self) -> Option<i32> {
        match self {
            Self::Checkmated => Some(0),
            Self::Score(Score::MateIn(moves)) => Some(moves),
            Self::Score(Score::Centipawns(_)) => None,
        }
    }

    /// The score from White's side in pawns, or the mate, as people read it: `+1.3`, `#-2`.
    pub fn text_for_white(self, side_to_move: Color) -> String {
        let sign = if side_to_move == Color::White { 1 } else { -1 };
        match self {
            Self::Checkmated => if sign > 0 { "0-1" } else { "1-0" }.to_string(),
            Self::Score(Score::MateIn(moves)) => {
                let moves = moves * sign;
                if moves >= 0 { format!("#{moves}") } else { format!("#-{}", -moves) }
            }
            Self::Score(Score::Centipawns(cp)) => format!("{:+.1}", f64::from(cp * sign) / 100.0),
        }
    }
}

/// A chance of winning from 0 to 100, with draws counting half.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct WinChance(f64);

impl WinChance {
    pub const WON: Self = Self(100.0);
    pub const LOST: Self = Self(0.0);
    pub const EVEN: Self = Self(50.0);

    pub const fn percent(self) -> f64 {
        self.0
    }

    pub fn for_opponent(self) -> Self {
        Self(100.0 - self.0)
    }

    /// Percentage points lost going from `self` to `after`; never negative.
    pub fn drop_to(self, after: Self) -> f64 {
        (self.0 - after.0).max(0.0)
    }
}

/// How good a move was, from the engine's best down to a blunder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MoveClass {
    /// A known opening move.
    Book,
    Best,
    Good,
    Inaccuracy,
    Mistake,
    Blunder,
}

impl MoveClass {
    /// Judges a move that was not the engine's choice by the winning chance it gave away.
    pub fn from_drop(drop: f64) -> Self {
        if drop >= BLUNDER_DROP {
            Self::Blunder
        } else if drop >= MISTAKE_DROP {
            Self::Mistake
        } else if drop >= INACCURACY_DROP {
            Self::Inaccuracy
        } else if drop < NEAR_BEST_DROP {
            Self::Best
        } else {
            Self::Good
        }
    }

    pub const fn is_error(self) -> bool {
        matches!(self, Self::Inaccuracy | Self::Mistake | Self::Blunder)
    }

    /// Mistakes and blunders: the moves worth turning into puzzles.
    pub const fn is_serious(self) -> bool {
        matches!(self, Self::Mistake | Self::Blunder)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Book => "Book move",
            Self::Best => "Best move",
            Self::Good => "Good move",
            Self::Inaccuracy => "Inaccuracy",
            Self::Mistake => "Mistake",
            Self::Blunder => "Blunder",
        }
    }

    /// The annotation symbol written after the move.
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Book | Self::Best | Self::Good => "",
            Self::Inaccuracy => "?!",
            Self::Mistake => "?",
            Self::Blunder => "??",
        }
    }
}

/// Lichess's accuracy for one move: 100 when nothing was lost, falling fast with the drop.
pub fn move_accuracy(drop: f64) -> f64 {
    (103.166_8 * (-0.043_54 * drop).exp() - 3.166_9).clamp(0.0, 100.0)
}

/// Accuracy over several moves: the mean of the arithmetic and harmonic means, so a few
/// blunders weigh more than in a plain average.
pub fn overall_accuracy(accuracies: &[f64]) -> Option<f64> {
    if accuracies.is_empty() {
        return None;
    }
    let n = accuracies.len() as f64;
    let arithmetic = accuracies.iter().sum::<f64>() / n;
    let harmonic = n / accuracies.iter().map(|a| 1.0 / a.max(1.0)).sum::<f64>();
    Some(f64::midpoint(arithmetic, harmonic))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Phase {
    Opening,
    Middlegame,
    Endgame,
}

impl Phase {
    pub const ALL: [Self; 3] = [Self::Opening, Self::Middlegame, Self::Endgame];

    /// The phase of a position reached after `ply` half-moves of a game.
    pub fn of(board: &Board, ply: usize) -> Self {
        if non_pawn_material(board, Color::White) + non_pawn_material(board, Color::Black) <= ENDGAME_MATERIAL {
            Self::Endgame
        } else if ply < OPENING_PLIES {
            Self::Opening
        } else {
            Self::Middlegame
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Opening => "Opening",
            Self::Middlegame => "Middlegame",
            Self::Endgame => "Endgame",
        }
    }
}

/// The usual teaching values: pawn 1, knight and bishop 3, rook 5, queen 9.
pub const fn piece_points(kind: PieceKind) -> i32 {
    match kind {
        PieceKind::Pawn => 1,
        PieceKind::Knight | PieceKind::Bishop => 3,
        PieceKind::Rook => 5,
        PieceKind::Queen => 9,
        PieceKind::King => 0,
    }
}

const COUNTED_KINDS: [PieceKind; 5] =
    [PieceKind::Pawn, PieceKind::Knight, PieceKind::Bishop, PieceKind::Rook, PieceKind::Queen];

/// `color`'s material in points.
pub fn material(board: &Board, color: Color) -> i32 {
    COUNTED_KINDS
        .iter()
        .map(|&kind| piece_count(board, kind, color) * piece_points(kind))
        .sum()
}

fn non_pawn_material(board: &Board, color: Color) -> i32 {
    material(board, color) - piece_count(board, PieceKind::Pawn, color)
}

pub fn piece_count(board: &Board, kind: PieceKind, color: Color) -> i32 {
    board.piece_bb[crate::types::Piece::from_kind(kind, color).index()].count_ones() as i32
}

/// `color`'s material lead in points.
pub fn material_balance(board: &Board, color: Color) -> i32 {
    material(board, color) - material(board, color.other())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::START_FEN;

    #[test]
    fn an_even_score_is_an_even_chance_and_mates_are_certain() {
        assert!((Eval::DRAWN.win_chance().percent() - 50.0).abs() < 1e-9);
        assert_eq!(Eval::Score(Score::MateIn(3)).win_chance(), WinChance::WON);
        assert_eq!(Eval::Score(Score::MateIn(-1)).win_chance(), WinChance::LOST);
        assert_eq!(Eval::Checkmated.win_chance(), WinChance::LOST);
    }

    #[test]
    fn a_piece_up_is_a_large_but_not_certain_edge() {
        let chance = Eval::Score(Score::Centipawns(300)).win_chance().percent();

        assert!((70.0..85.0).contains(&chance), "{chance}");
    }

    #[test]
    fn the_opponents_chance_is_the_complement() {
        let eval = Eval::Score(Score::Centipawns(150));

        let mine = eval.win_chance_for(Color::White, Color::White).percent();
        let theirs = eval.win_chance_for(Color::White, Color::Black).percent();

        assert!((mine + theirs - 100.0).abs() < 1e-9);
    }

    #[test]
    fn bigger_drops_are_worse_classes() {
        assert_eq!(MoveClass::from_drop(0.2), MoveClass::Best);
        assert_eq!(MoveClass::from_drop(3.0), MoveClass::Good);
        assert_eq!(MoveClass::from_drop(6.0), MoveClass::Inaccuracy);
        assert_eq!(MoveClass::from_drop(12.0), MoveClass::Mistake);
        assert_eq!(MoveClass::from_drop(40.0), MoveClass::Blunder);
    }

    #[test]
    fn accuracy_is_perfect_without_loss_and_drags_down_with_blunders() {
        assert!((move_accuracy(0.0) - 100.0).abs() < 0.01);
        let steady = overall_accuracy(&[90.0, 90.0, 90.0, 90.0]).unwrap();
        let blundery = overall_accuracy(&[100.0, 100.0, 100.0, 5.0]).unwrap();

        assert!((steady - 90.0).abs() < 1e-9);
        assert!(blundery < 60.0, "{blundery}");
    }

    #[test]
    fn scores_read_from_whites_side() {
        assert_eq!(Eval::Score(Score::Centipawns(130)).text_for_white(Color::Black), "-1.3");
        assert_eq!(Eval::Score(Score::MateIn(2)).text_for_white(Color::White), "#2");
        assert_eq!(Eval::Score(Score::MateIn(2)).text_for_white(Color::Black), "#-2");
    }

    #[test]
    fn phases_follow_move_number_and_material() {
        let start = Board::from_fen(START_FEN).unwrap();
        let rook_ending = Board::from_fen("4k3/pp3r2/8/8/8/8/PP3R2/4K3 w - - 0 30").unwrap();

        assert_eq!(Phase::of(&start, 0), Phase::Opening);
        assert_eq!(Phase::of(&start, 30), Phase::Middlegame);
        assert_eq!(Phase::of(&rook_ending, 60), Phase::Endgame);
        assert_eq!(material_balance(&rook_ending, Color::White), 0);
    }
}
