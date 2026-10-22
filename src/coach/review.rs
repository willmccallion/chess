//! Judging every move of a game from engine analysis, and explaining what went wrong.

use super::judgement::{
    Eval, MoveClass, Phase, WinChance, material_balance, move_accuracy, overall_accuracy,
};
use crate::board::Board;
use crate::notation::{line_in_san, numbered, san_of};
use crate::see::see;
use crate::types::{Color, Move, MoveList, PieceKind, sq_to_str};

/// A capture whose exchange nets at least this much (centipawns) wins material outright.
const WINNING_CAPTURE_SEE: i32 = 150;
/// Material (in points) a line must win before it counts as winning material.
const MATERIAL_WIN_POINTS: i32 = 2;
/// How far along an engine line material changes are followed.
const LINE_MATERIAL_PLIES: usize = 8;
/// Opening moves are only checked against the book this early.
const BOOK_PLIES: usize = 24;

/// The engine's view of one position of a game.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysedPosition {
    pub eval: Eval,
    /// The engine's best line from here; empty when the game is over.
    pub best_line: Vec<Move>,
}

/// Why a move was an error, as a coach would put it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The moved piece went to a square where it can be won.
    MovedIntoCapture {
        piece: PieceKind,
        square: u8,
        capture: String,
    },
    /// Another piece lost its defence and can be won.
    LeftHanging {
        piece: PieceKind,
        square: u8,
        capture: String,
    },
    /// A piece that was already attacked stayed attacked.
    IgnoredThreat {
        piece: PieceKind,
        square: u8,
        capture: String,
    },
    /// The opponent gets a combination (a fork, pin or the like) that wins material.
    AllowedTactic {
        points: i32,
        first_move: String,
    },
    AllowedMate {
        moves: i32,
        first_move: String,
    },
    MissedMate {
        moves: i32,
        first_move: String,
    },
    MissedMaterial {
        points: i32,
        first_move: String,
    },
    /// No tactic: the move was simply weaker than the alternative.
    Positional,
}

/// The kind of a [`Cause`], for counting mistakes across games.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CauseKind {
    HungPiece,
    IgnoredThreat,
    AllowedTactic,
    AllowedMate,
    MissedMate,
    MissedMaterial,
    Positional,
}

impl CauseKind {
    pub const ALL: [Self; 7] = [
        Self::HungPiece,
        Self::IgnoredThreat,
        Self::AllowedTactic,
        Self::AllowedMate,
        Self::MissedMate,
        Self::MissedMaterial,
        Self::Positional,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::HungPiece => "Hanging pieces",
            Self::IgnoredThreat => "Missed threats",
            Self::AllowedTactic => "Walked into tactics",
            Self::AllowedMate => "Allowed checkmate",
            Self::MissedMate => "Missed checkmates",
            Self::MissedMaterial => "Missed free material",
            Self::Positional => "Weaker plans",
        }
    }

    /// The habit that fixes this kind of mistake.
    pub const fn advice(self) -> &'static str {
        match self {
            Self::HungPiece => {
                "Before you let go of a piece, look at its new square: can it be taken? Is it defended? \
                 And check the piece you moved away was not guarding something."
            }
            Self::IgnoredThreat => {
                "After every opponent move, ask: what does that move attack? Deal with the threat \
                 before carrying on with your own plan."
            }
            Self::AllowedTactic => {
                "Before moving, look at every check, capture and threat your opponent will have. \
                 Forks and pins only work on pieces that are lined up or loose."
            }
            Self::AllowedMate => {
                "Keep an eye on your king: count the checks your opponent has, and keep a pawn \
                 shield and an escape square."
            }
            Self::MissedMate => {
                "Start every move by looking at your own checks. Forcing moves come first, and \
                 sometimes one of them is mate."
            }
            Self::MissedMaterial => {
                "Look for undefended enemy pieces each move. If something is attacked more times \
                 than it is defended, you can usually win it."
            }
            Self::Positional => {
                "Make every move improve a piece: develop, castle, put rooks on open files, and \
                 move pieces towards the centre."
            }
        }
    }
}

impl Cause {
    pub const fn kind(&self) -> CauseKind {
        match self {
            Self::MovedIntoCapture { .. } | Self::LeftHanging { .. } => CauseKind::HungPiece,
            Self::IgnoredThreat { .. } => CauseKind::IgnoredThreat,
            Self::AllowedTactic { .. } => CauseKind::AllowedTactic,
            Self::AllowedMate { .. } => CauseKind::AllowedMate,
            Self::MissedMate { .. } => CauseKind::MissedMate,
            Self::MissedMaterial { .. } => CauseKind::MissedMaterial,
            Self::Positional => CauseKind::Positional,
        }
    }

    /// One or two sentences saying what went wrong.
    pub fn explain(&self) -> String {
        match self {
            Self::MovedIntoCapture {
                piece,
                square,
                capture,
            } => format!(
                "Your {} on {} can be taken: {capture} wins it.",
                piece_name(*piece),
                sq_to_str(usize::from(*square))
            ),
            Self::LeftHanging {
                piece,
                square,
                capture,
            } => format!(
                "This left your {} on {} without enough defence: {capture} wins it.",
                piece_name(*piece),
                sq_to_str(usize::from(*square))
            ),
            Self::IgnoredThreat {
                piece,
                square,
                capture,
            } => format!(
                "Your {} on {} was already attacked and is still in danger: {capture} wins it.",
                piece_name(*piece),
                sq_to_str(usize::from(*square))
            ),
            Self::AllowedTactic { points, first_move } => format!(
                "This allows {first_move}, a tactic that wins {} for your opponent.",
                points_text(*points)
            ),
            Self::AllowedMate { moves, first_move } => {
                format!("This allows checkmate in {moves}, starting with {first_move}.")
            }
            Self::MissedMate { moves, first_move } => {
                format!("You had checkmate in {moves}, starting with {first_move}.")
            }
            Self::MissedMaterial { points, first_move } => {
                format!("{first_move} would have won {}.", points_text(*points))
            }
            Self::Positional => {
                "No tactic here, but the engine's move kept a better position.".to_string()
            }
        }
    }
}

pub(super) fn points_text(points: i32) -> String {
    match points {
        1 => "a pawn".to_string(),
        2 => "the exchange or two pawns".to_string(),
        3 => "a piece".to_string(),
        5 => "a rook".to_string(),
        9 => "the queen".to_string(),
        _ => format!("{points} points of material"),
    }
}

pub const fn piece_name(kind: PieceKind) -> &'static str {
    match kind {
        PieceKind::Pawn => "pawn",
        PieceKind::Knight => "knight",
        PieceKind::Bishop => "bishop",
        PieceKind::Rook => "rook",
        PieceKind::Queen => "queen",
        PieceKind::King => "king",
    }
}

/// The verdict on one move of a game.
#[derive(Clone, Debug, PartialEq)]
pub struct MoveReview {
    /// Half-moves played before this one.
    pub ply: usize,
    pub mover: Color,
    pub played: Move,
    /// The move in SAN with its number, like `12... Nf6`.
    pub numbered_san: String,
    pub san: String,
    pub class: MoveClass,
    /// The mover's winning chance before and after the move.
    pub before: WinChance,
    pub after: WinChance,
    /// The engine's choice when it differs from the move played.
    pub best: Option<Move>,
    pub best_san: Option<String>,
    pub best_line: String,
    /// What the engine expects to follow the move played.
    pub reply_line: String,
    pub cause: Option<Cause>,
    pub phase: Phase,
}

impl MoveReview {
    pub fn drop(&self) -> f64 {
        self.before.drop_to(self.after)
    }

    pub fn accuracy(&self) -> f64 {
        move_accuracy(self.drop())
    }
}

/// Reviews the moves of a game that have analysis for the positions before and after them;
/// `analysis[i]` is the position after `moves[..i]`. `is_book` says whether a move is a
/// known opening move.
pub fn review_moves(
    start: &Board,
    moves: &[Move],
    analysis: &[AnalysedPosition],
    is_book: &dyn Fn(&Board, Move) -> bool,
) -> Vec<MoveReview> {
    let mut board = start.without_eval_state();
    let mut reviews = Vec::new();
    let mut still_in_book = true;
    for (ply, (&played, pair)) in moves.iter().zip(analysis.windows(2)).enumerate() {
        let [before, after] = pair else {
            break;
        };
        still_in_book &= ply < BOOK_PLIES && is_book(&board, played);
        let Some(review) = review_move(&board, ply, played, before, after, still_in_book) else {
            break;
        };
        reviews.push(review);
        board.make_move(played);
    }
    reviews
}

fn review_move(
    board: &Board,
    ply: usize,
    played: Move,
    before: &AnalysedPosition,
    after: &AnalysedPosition,
    in_book: bool,
) -> Option<MoveReview> {
    let san = san_of(board, played)?;
    let mover = board.turn;
    let mut after_board = board.clone();
    after_board.make_move(played);
    let chance_before = before.eval.win_chance();
    let chance_after = after.eval.win_chance_for(mover.other(), mover);
    let engine_best = before.best_line.first().copied();
    let drop = chance_before.drop_to(chance_after);
    let class = if in_book && !MoveClass::from_drop(drop).is_error() {
        MoveClass::Book
    } else if engine_best == Some(played) {
        MoveClass::Best
    } else {
        MoveClass::from_drop(drop)
    };
    let best = engine_best.filter(|&m| m != played);
    let cause = class
        .is_error()
        .then(|| find_cause(board, played, &after_board, before, after));
    Some(MoveReview {
        ply,
        mover,
        played,
        numbered_san: numbered(board, &san, true),
        san,
        class,
        before: chance_before,
        after: chance_after,
        best,
        best_san: best.and_then(|m| san_of(board, m)),
        best_line: line_in_san(board, &before.best_line),
        reply_line: line_in_san(&after_board, &after.best_line),
        cause,
        phase: Phase::of(board, ply),
    })
}

/// Explains an error by `played` in `board`, which led to `after_board`.
fn find_cause(
    board: &Board,
    played: Move,
    after_board: &Board,
    before: &AnalysedPosition,
    after: &AnalysedPosition,
) -> Cause {
    let mover = board.turn;
    let had_mate = before.eval.mate_in().filter(|&n| n > 0);
    let still_mating = after.eval.mate_in().is_some_and(|n| n < 0);
    if let Some(moves) = had_mate
        && !still_mating
    {
        return Cause::MissedMate {
            moves,
            first_move: first_move_text(board, &before.best_line),
        };
    }
    let was_being_mated = before.eval.mate_in().is_some_and(|n| n < 0);
    if let Some(moves) = after.eval.mate_in().filter(|&n| n > 0)
        && !was_being_mated
    {
        return Cause::AllowedMate {
            moves,
            first_move: first_move_text(after_board, &after.best_line),
        };
    }
    let opponent_gain = material_gain_along(after_board, &after.best_line, mover.other());
    if opponent_gain >= MATERIAL_WIN_POINTS {
        return lost_material_cause(board, played, after_board, &after.best_line, opponent_gain);
    }
    let best_gain = material_gain_along(board, &before.best_line, mover);
    let played_line: Vec<Move> = std::iter::once(played)
        .chain(after.best_line.iter().copied())
        .collect();
    let played_gain = material_gain_along(board, &played_line, mover);
    if best_gain >= MATERIAL_WIN_POINTS && best_gain - played_gain >= MATERIAL_WIN_POINTS {
        return Cause::MissedMaterial {
            points: best_gain - played_gain,
            first_move: first_move_text(board, &before.best_line),
        };
    }
    Cause::Positional
}

/// Names how the opponent's best line after `played` wins material.
fn lost_material_cause(
    board: &Board,
    played: Move,
    after_board: &Board,
    reply_line: &[Move],
    points: i32,
) -> Cause {
    let Some(&reply) = reply_line.first() else {
        return Cause::Positional;
    };
    let capture = first_move_text(after_board, reply_line);
    let target = reply.to;
    let piece = after_board.piece_on[usize::from(target)].kind();
    let wins_outright =
        reply.capture && !reply.en_passant && see(after_board, reply) >= WINNING_CAPTURE_SEE;
    let Some(piece) = piece.filter(|_| wins_outright) else {
        return Cause::AllowedTactic {
            points,
            first_move: capture,
        };
    };
    if target == played.to {
        Cause::MovedIntoCapture {
            piece,
            square: target,
            capture,
        }
    } else if was_already_attacked(board, target) {
        Cause::IgnoredThreat {
            piece,
            square: target,
            capture,
        }
    } else {
        Cause::LeftHanging {
            piece,
            square: target,
            capture,
        }
    }
}

/// Whether the opponent could already have won the piece on `square` had it been their move.
fn was_already_attacked(board: &Board, square: u8) -> bool {
    let mover = board.turn;
    let king = board.king_square(mover);
    if king < 64 && board.is_square_attacked(king as i32, mover.other()) {
        return false;
    }
    let mut passed = board.clone();
    passed.make_null_move();
    let mut legal = MoveList::new();
    passed.generate_legal_moves(&mut legal);
    legal
        .as_slice()
        .iter()
        .any(|&m| m.to == square && m.capture && see(&passed, m) >= WINNING_CAPTURE_SEE)
}

/// How many points of material `side` gains along `line` from `board`, measured once the
/// captures in the line have settled.
pub(super) fn material_gain_along(board: &Board, line: &[Move], side: Color) -> i32 {
    let start = material_balance(board, side);
    let mut position = board.clone();
    let mut settled = start;
    for (i, &m) in line.iter().take(LINE_MATERIAL_PLIES).enumerate() {
        if san_of(&position, m).is_none() {
            break;
        }
        position.make_move(m);
        let next_is_capture = line.get(i + 1).is_some_and(|next| next.capture);
        if !next_is_capture || i + 1 == LINE_MATERIAL_PLIES {
            settled = material_balance(&position, side);
        }
    }
    settled - start
}

fn first_move_text(board: &Board, line: &[Move]) -> String {
    line.first()
        .and_then(|&m| san_of(board, m).map(|san| numbered(board, &san, true)))
        .unwrap_or_default()
}

/// Accuracy and error counts for one side over a set of reviewed moves.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SideSummary {
    pub accuracy: Option<f64>,
    pub inaccuracies: usize,
    pub mistakes: usize,
    pub blunders: usize,
}

pub fn summarise(reviews: &[MoveReview], side: Color) -> SideSummary {
    let own: Vec<&MoveReview> = reviews.iter().filter(|r| r.mover == side).collect();
    let count = |class: MoveClass| own.iter().filter(|r| r.class == class).count();
    let accuracies: Vec<f64> = own.iter().map(|r| r.accuracy()).collect();
    SideSummary {
        accuracy: overall_accuracy(&accuracies),
        inaccuracies: count(MoveClass::Inaccuracy),
        mistakes: count(MoveClass::Mistake),
        blunders: count(MoveClass::Blunder),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notation::parse_san_line;
    use crate::search::Score;

    fn cp(cp: i32) -> Eval {
        Eval::Score(Score::Centipawns(cp))
    }

    fn analysed(board: &Board, eval: Eval, line: &[&str]) -> AnalysedPosition {
        AnalysedPosition {
            eval,
            best_line: parse_san_line(board, line).unwrap(),
        }
    }

    fn after(board: &Board, san: &str) -> Board {
        let mut next = board.clone();
        next.make_move(parse_san_line(board, &[san]).unwrap()[0]);
        next
    }

    fn review_one(
        fen: &str,
        played: &str,
        before: (Eval, &[&str]),
        after_eval: (Eval, &[&str]),
    ) -> MoveReview {
        let board = Board::from_fen(fen).unwrap();
        let next = after(&board, played);
        let analysis = [
            analysed(&board, before.0, before.1),
            analysed(&next, after_eval.0, after_eval.1),
        ];
        let moves = parse_san_line(&board, &[played]).unwrap();
        review_moves(&board, &moves, &analysis, &|_, _| false).remove(0)
    }

    #[test]
    fn the_engines_move_is_best() {
        let review = review_one(
            crate::types::START_FEN,
            "e4",
            (cp(30), &["e4", "e5"]),
            (cp(-30), &["e5"]),
        );

        assert_eq!(review.class, MoveClass::Best);
        assert_eq!(review.best, None);
        assert_eq!(review.cause, None);
    }

    #[test]
    fn moving_a_knight_where_a_pawn_takes_it_is_a_hung_piece() {
        let review = review_one(
            "rnbqkbnr/ppp2ppp/8/3p4/3Pp3/8/PPP2PPP/RNBQKBNR w KQkq - 0 4",
            "Nf3",
            (cp(0), &["c4"]),
            (cp(350), &["exf3", "Qxf3"]),
        );

        assert_eq!(review.class, MoveClass::Blunder);
        assert_eq!(
            review.cause,
            Some(Cause::MovedIntoCapture {
                piece: PieceKind::Knight,
                square: 21,
                capture: "4... exf3".into()
            })
        );
    }

    #[test]
    fn leaving_an_attacked_piece_where_it_is_is_an_ignored_threat() {
        let review = review_one(
            "rnbqkb1r/pppp1ppp/8/4p3/3Bn3/8/PPP1PPPP/RN1QKBNR w KQkq - 0 4",
            "h3",
            (cp(0), &["Be3"]),
            (cp(300), &["exd4"]),
        );

        assert_eq!(
            review.cause,
            Some(Cause::IgnoredThreat {
                piece: PieceKind::Bishop,
                square: 27,
                capture: "4... exd4".into()
            })
        );
    }

    #[test]
    fn allowing_a_fork_is_walking_into_a_tactic() {
        let review = review_one(
            "4k3/8/8/8/1n6/8/8/R2QK3 w - - 0 1",
            "Qg4",
            (cp(0), &["Qd2"]),
            (cp(500), &["Nc2+", "Kd2", "Nxa1"]),
        );

        assert_eq!(
            review.cause,
            Some(Cause::AllowedTactic {
                points: 5,
                first_move: "1... Nc2+".into()
            })
        );
    }

    #[test]
    fn losing_a_forced_mate_is_a_missed_mate() {
        let review = review_one(
            "6k1/5ppp/8/8/8/8/5PPP/R5K1 w - - 0 1",
            "h3",
            (Eval::Score(Score::MateIn(1)), &["Ra8#"]),
            (cp(-500), &["h6"]),
        );

        assert_eq!(
            review.cause,
            Some(Cause::MissedMate {
                moves: 1,
                first_move: "1. Ra8#".into()
            })
        );
    }

    #[test]
    fn allowing_fools_mate_is_named() {
        let review = review_one(
            "rnbqkbnr/pppp1ppp/8/4p3/8/5P2/PPPPP1PP/RNBQKBNR w KQkq - 0 2",
            "g4",
            (cp(-80), &["e4"]),
            (Eval::Score(Score::MateIn(1)), &["Qh4#"]),
        );

        assert_eq!(
            review.cause,
            Some(Cause::AllowedMate {
                moves: 1,
                first_move: "2... Qh4#".into()
            })
        );
    }

    #[test]
    fn not_taking_a_free_queen_is_missed_material() {
        let review = review_one(
            "4k3/8/8/3q4/8/8/8/3RK3 w - - 0 1",
            "Ke2",
            (cp(900), &["Rxd5"]),
            (cp(-400), &["Qe4+"]),
        );

        assert!(
            matches!(review.cause, Some(Cause::MissedMaterial { points: 9, .. })),
            "{:?}",
            review.cause
        );
    }

    #[test]
    fn known_opening_moves_are_book_until_the_first_new_move() {
        let board = Board::from_fen(crate::types::START_FEN).unwrap();
        let moves = parse_san_line(&board, &["d4", "d5", "c4"]).unwrap();
        let positions: Vec<Board> = (0..=moves.len())
            .map(|n| {
                let mut b = board.clone();
                moves[..n].iter().for_each(|&m| {
                    b.make_move(m);
                });
                b
            })
            .collect();
        let analysis: Vec<AnalysedPosition> = positions
            .iter()
            .map(|_| AnalysedPosition {
                eval: cp(20),
                best_line: Vec::new(),
            })
            .collect();
        let d4 = moves[0];

        let reviews = review_moves(&board, &moves, &analysis, &|_, m| m == d4);

        assert_eq!(reviews[0].class, MoveClass::Book);
        assert_ne!(reviews[1].class, MoveClass::Book);
    }

    #[test]
    fn summaries_count_one_sides_errors() {
        let blunder = review_one(
            "6k1/5ppp/8/8/8/8/5PPP/R5K1 w - - 0 1",
            "h3",
            (Eval::Score(Score::MateIn(1)), &["Ra8#"]),
            (cp(500), &["h6"]),
        );

        let white = summarise(std::slice::from_ref(&blunder), Color::White);
        let black = summarise(&[blunder], Color::Black);

        assert_eq!(white.blunders, 1);
        assert!(white.accuracy.unwrap() < 10.0);
        assert_eq!(black.accuracy, None);
    }
}
