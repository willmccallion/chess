//! Why a move went wrong, shown concretely: where the line after it leads, where the better
//! move's line leads, and how the positions after the two moves differ.

use super::judgement::Phase;
use super::review::{material_gain_along, points_text};
use crate::board::Board;
use crate::game_runner::{GameEndReason, position_outcome};
use crate::notation::{line_in_san, san_of};
use crate::types::{
    BK_CASTLE, BQ_CASTLE, Color, Move, MoveList, Piece, PieceKind, WK_CASTLE, WQ_CASTLE,
};

/// Moves of a line shown in text; the rest is summed up in its outcome.
const SHOWN_PLIES: usize = 8;
/// A mobility difference smaller than this is too small to mention.
const MOBILITY_NOTE: i32 = 6;
const MAX_NOTES: usize = 3;

/// A line in words: its first moves and where it ends up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineStory {
    pub moves: String,
    pub outcome: String,
}

/// Where `line` from `board` leads, told from `side`'s point of view by colour name.
pub fn line_story(board: &Board, line: &[Move], side: Color) -> Option<LineStory> {
    if line.is_empty() {
        return None;
    }
    let shown = &line[..line.len().min(SHOWN_PLIES)];
    let mut moves = line_in_san(board, shown);
    if line.len() > shown.len() {
        moves.push_str(" …");
    }
    Some(LineStory {
        moves,
        outcome: outcome_of(board, line, side),
    })
}

fn outcome_of(board: &Board, line: &[Move], side: Color) -> String {
    let mut end = board.without_eval_state();
    for &m in line {
        if san_of(&end, m).is_none() {
            break;
        }
        end.make_move(m);
    }
    if let Some((_, GameEndReason::Checkmate)) = position_outcome(&end) {
        return format!("{} is checkmated", color_name(end.turn));
    }
    let gain = material_gain_along(board, line, side);
    let (winner, points) = if gain >= 0 {
        (side, gain)
    } else {
        (side.other(), -gain)
    };
    if points == 0 {
        "material stays level".to_string()
    } else {
        format!("{} wins {}", color_name(winner), points_text(points))
    }
}

const fn color_name(color: Color) -> &'static str {
    match color {
        Color::White => "White",
        Color::Black => "Black",
    }
}

/// How the position after `played` is worse than after `better`, beyond the evaluation:
/// castling, the king's pawn cover, pawn weaknesses, development and room for the pieces.
pub fn positional_differences(
    before: &Board,
    played: Move,
    better: Move,
    phase: Phase,
) -> Vec<String> {
    let mover = before.turn;
    let (Some(played_san), Some(better_san)) = (san_of(before, played), san_of(before, better))
    else {
        return Vec::new();
    };
    let after = |m: Move| {
        let mut b = before.without_eval_state();
        b.make_move(m);
        b
    };
    let (worse, good) = (after(played), after(better));
    let mut notes = Vec::new();
    let rights = |b: &Board| castling_rights(b, mover);
    if rights(&worse) < rights(before) && !played.castle && rights(&good) == rights(before) {
        notes.push(format!(
            "{played_san} gives up the right to castle; {better_san} keeps it."
        ));
    }
    if king_cover(&worse, mover) < king_cover(&good, mover) {
        notes.push(format!(
            "{played_san} loosens the pawns in front of the king."
        ));
    }
    if doubled_pawns(&worse, mover) > doubled_pawns(&good, mover) {
        notes.push(format!("{played_san} leaves doubled pawns."));
    }
    if isolated_pawns(&worse, mover) > isolated_pawns(&good, mover) {
        notes.push(format!("{played_san} leaves an isolated pawn."));
    }
    if phase == Phase::Opening && develops(before, better) && !develops(before, played) {
        notes.push(format!(
            "{better_san} brings a new piece into play; {played_san} doesn't."
        ));
    }
    let (own_worse, own_good) = (mobility(&worse, mover), mobility(&good, mover));
    if own_good - own_worse >= MOBILITY_NOTE {
        notes.push(format!(
            "After {better_san} the pieces have more room: {own_good} moves against {own_worse}."
        ));
    }
    let (their_worse, their_good) = (
        mobility(&worse, mover.other()),
        mobility(&good, mover.other()),
    );
    if their_worse - their_good >= MOBILITY_NOTE {
        notes.push(format!(
            "{played_san} gives the opponent's pieces more room: {their_worse} moves against {their_good}."
        ));
    }
    notes.truncate(MAX_NOTES);
    notes
}

fn castling_rights(board: &Board, color: Color) -> u32 {
    let mask = match color {
        Color::White => WK_CASTLE | WQ_CASTLE,
        Color::Black => BK_CASTLE | BQ_CASTLE,
    };
    (board.castle & mask).count_ones()
}

/// Own pawns on the two ranks in front of a king on its back rank, on its file and beside it.
fn king_cover(board: &Board, color: Color) -> u32 {
    let king = board.king_square(color);
    if king >= 64 {
        return 0;
    }
    let (file, rank) = (king % 8, king / 8);
    let (home, step): (u32, i32) = if color == Color::White {
        (0, 1)
    } else {
        (7, -1)
    };
    if rank != home {
        return 0;
    }
    let pawn = Piece::from_kind(PieceKind::Pawn, color);
    let mut cover = 0;
    for f in file.saturating_sub(1)..=(file + 1).min(7) {
        for distance in 1..=2 {
            let r = rank as i32 + step * distance;
            if board.piece_on[(r as u32 * 8 + f) as usize] == pawn {
                cover += 1;
            }
        }
    }
    cover
}

fn pawns_per_file(board: &Board, color: Color) -> [u32; 8] {
    let pawns = board.piece_bb[Piece::from_kind(PieceKind::Pawn, color).index()];
    std::array::from_fn(|file| (pawns & (0x0101_0101_0101_0101u64 << file)).count_ones())
}

fn doubled_pawns(board: &Board, color: Color) -> u32 {
    pawns_per_file(board, color)
        .iter()
        .map(|&n| n.saturating_sub(1))
        .sum()
}

fn isolated_pawns(board: &Board, color: Color) -> u32 {
    let files = pawns_per_file(board, color);
    (0..8)
        .filter(|&f| {
            let left = f > 0 && files[f - 1] > 0;
            let right = f < 7 && files[f + 1] > 0;
            !left && !right
        })
        .map(|f| files[f])
        .sum()
}

/// Whether `m` moves a knight or bishop off its starting square.
fn develops(board: &Board, m: Move) -> bool {
    let piece = board.piece_on[usize::from(m.from)];
    let home_rank = if board.turn == Color::White { 0 } else { 7 };
    matches!(piece.kind(), Some(PieceKind::Knight | PieceKind::Bishop)) && m.from / 8 == home_rank
}

/// How many moves `color`'s pieces would have if it were their turn.
fn mobility(board: &Board, color: Color) -> i32 {
    let mut b = board.without_eval_state();
    if b.turn != color {
        b.make_null_move();
    }
    let mut moves = MoveList::new();
    b.generate_pseudo_legal_moves(&mut moves);
    moves.len() as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notation::parse_san_line;
    use crate::types::START_FEN;

    fn board(fen: &str) -> Board {
        Board::from_fen(fen).unwrap()
    }

    fn moves(b: &Board, sans: &[&str]) -> Vec<Move> {
        parse_san_line(b, sans).unwrap()
    }

    #[test]
    fn a_line_that_wins_a_piece_says_so() {
        let b = board("rnbqkbnr/ppp2ppp/8/3p4/3Pp3/5N2/PPP2PPP/RNBQKB1R b KQkq - 1 4");
        let line = moves(&b, &["exf3", "Qxf3"]);

        let story = line_story(&b, &line, Color::White).unwrap();

        assert_eq!(story.moves, "4... exf3 5. Qxf3");
        assert_eq!(story.outcome, "Black wins the exchange or two pawns");
    }

    #[test]
    fn a_line_ending_in_mate_says_who_is_mated() {
        let b = board("rnbqkbnr/pppp1ppp/8/4p3/6P1/5P2/PPPPP2P/RNBQKBNR b KQkq g3 0 2");

        let story = line_story(&b, &moves(&b, &["Qh4#"]), Color::White).unwrap();

        assert_eq!(story.outcome, "White is checkmated");
    }

    #[test]
    fn long_lines_are_cut_short() {
        let b = board(START_FEN);
        let line = moves(
            &b,
            &[
                "d4", "d5", "c4", "e6", "Nc3", "Nf6", "Bg5", "Be7", "e3", "O-O",
            ],
        );

        let story = line_story(&b, &line, Color::White).unwrap();

        assert!(story.moves.ends_with("4. Bg5 Be7 …"), "{}", story.moves);
        assert_eq!(story.outcome, "material stays level");
    }

    #[test]
    fn walking_the_king_loses_castling_against_developing() {
        let b = board("rnbqkbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNBQKBNR w KQkq - 0 2");
        let [ke2, nf3] = moves_pair(&b, "Ke2", "Nf3");

        let notes = positional_differences(&b, ke2, nf3, Phase::Opening);

        assert!(
            notes
                .iter()
                .any(|n| n.contains("gives up the right to castle")),
            "{notes:?}"
        );
        assert!(
            notes
                .iter()
                .any(|n| n.contains("brings a new piece into play")),
            "{notes:?}"
        );
    }

    #[test]
    fn pushing_a_pawn_in_front_of_the_castled_king_loosens_its_cover() {
        let b = board("r1bq1rk1/ppp2ppp/2np1n2/2b1p3/2B1P3/2NP1N2/PPP2PPP/R1BQ1RK1 w - - 0 7");
        let [g4, bg5] = moves_pair(&b, "g4", "Bg5");

        let notes = positional_differences(&b, g4, bg5, Phase::Middlegame);

        assert!(
            notes.iter().any(|n| n.contains("loosens the pawns")),
            "{notes:?}"
        );
    }

    #[test]
    fn recapturing_with_a_pawn_that_doubles_is_noted() {
        let b = board("rn1qkbnr/ppp2ppp/3p4/4p3/4P3/5b2/PPPP1PPP/RNBQKB1R w KQkq - 0 4");
        let [gxf3, qxf3] = moves_pair(&b, "gxf3", "Qxf3");

        let notes = positional_differences(&b, gxf3, qxf3, Phase::Opening);

        assert!(
            notes.iter().any(|n| n.contains("doubled pawns")),
            "{notes:?}"
        );
    }

    #[test]
    fn identical_moves_make_no_notes() {
        let b = board(START_FEN);
        let [e4, _] = moves_pair(&b, "e4", "d4");

        assert!(positional_differences(&b, e4, e4, Phase::Opening).is_empty());
    }

    fn moves_pair(b: &Board, first: &str, second: &str) -> [Move; 2] {
        [moves(b, &[first])[0], moves(b, &[second])[0]]
    }
}
