//! Reading and writing moves the way people do: SAN lines and typed moves.

use crate::board::Board;
use crate::types::{Color, Move, MoveList, PieceKind};
use crate::uci_io::parse_uci_move;

/// Reads a move typed in UCI (`e7e8q`) or SAN (`Nf3`, `exd5`, `O-O`), ignoring check marks.
pub fn parse_typed_move(board: &Board, legal: &[Move], text: &str) -> Option<Move> {
    let text = text.trim();
    let mut scratch = board.without_eval_state();
    if let Some(m) = parse_uci_move(&mut scratch, text) {
        return Some(m);
    }
    let wanted = normalize_san(text);
    legal
        .iter()
        .copied()
        .find(|&m| normalize_san(&board.to_san(m, legal)) == wanted)
        .or_else(|| parse_san_loosely(board, legal, &wanted))
}

/// The SAN pieces of a move: what moves, any hint of where from, and where to.
struct SanParts {
    kind: PieceKind,
    from_file: Option<u8>,
    from_rank: Option<u8>,
    to: u8,
    promotion: Option<PieceKind>,
}

/// Matches SAN by its parts, so over-specified moves like `Nbd7` still read when only one
/// knight can reach d7.
fn parse_san_loosely(board: &Board, legal: &[Move], san: &str) -> Option<Move> {
    let parts = san_parts(san)?;
    let mut matching = legal.iter().copied().filter(|m| {
        board.piece_on[usize::from(m.from)].kind() == Some(parts.kind)
            && m.to == parts.to
            && !m.castle
            && m.promotion == parts.promotion
            && parts.from_file.is_none_or(|file| m.from % 8 == file)
            && parts.from_rank.is_none_or(|rank| m.from / 8 == rank)
    });
    let found = matching.next()?;
    matching.next().is_none().then_some(found)
}

fn san_parts(san: &str) -> Option<SanParts> {
    let (body, promotion) = match san.split_once('=') {
        Some((body, promo)) => (body, Some(piece_kind_of(promo.chars().next()?)?)),
        None => (san, None),
    };
    let mut chars: Vec<char> = body.chars().filter(|&c| c != 'x').collect();
    let kind = match chars.first() {
        Some(&c) if c.is_ascii_uppercase() => {
            chars.remove(0);
            piece_kind_of(c)?
        }
        _ => PieceKind::Pawn,
    };
    if chars.len() < 2 {
        return None;
    }
    let target = chars.split_off(chars.len() - 2);
    let to = square_of(target[0], target[1])?;
    let mut parts = SanParts { kind, from_file: None, from_rank: None, to, promotion };
    for c in chars {
        match c {
            'a'..='h' => parts.from_file = Some(c as u8 - b'a'),
            '1'..='8' => parts.from_rank = Some(c as u8 - b'1'),
            _ => return None,
        }
    }
    Some(parts)
}

fn piece_kind_of(letter: char) -> Option<PieceKind> {
    match letter {
        'N' => Some(PieceKind::Knight),
        'B' => Some(PieceKind::Bishop),
        'R' => Some(PieceKind::Rook),
        'Q' => Some(PieceKind::Queen),
        'K' => Some(PieceKind::King),
        _ => None,
    }
}

fn square_of(file: char, rank: char) -> Option<u8> {
    let file = ('a'..='h').contains(&file).then(|| file as u8 - b'a')?;
    let rank = ('1'..='8').contains(&rank).then(|| rank as u8 - b'1')?;
    Some(rank * 8 + file)
}

fn normalize_san(san: &str) -> String {
    san.chars()
        .filter(|c| !matches!(c, '+' | '#' | '!' | '?'))
        .map(|c| if c == '0' { 'O' } else { c })
        .collect()
}

/// The SAN of `m` in `board`, or None if it is not legal there.
pub fn san_of(board: &Board, m: Move) -> Option<String> {
    let mut legal = MoveList::new();
    board.generate_legal_moves(&mut legal);
    legal.as_slice().contains(&m).then(|| board.to_san(m, legal.as_slice()))
}

/// `moves` from `root` in numbered SAN, stopping at the first move that is not legal.
pub fn line_in_san(root: &Board, moves: &[Move]) -> String {
    let mut board = root.without_eval_state();
    let mut parts = Vec::new();
    for (i, &m) in moves.iter().enumerate() {
        let Some(san) = san_of(&board, m) else {
            break;
        };
        parts.push(numbered(&board, &san, i == 0));
        board.make_move(m);
    }
    parts.join(" ")
}

/// `san` with its move number when it needs one: always for White, and for Black when
/// it starts a line (`12... Nf6`).
pub fn numbered(board: &Board, san: &str, starts_line: bool) -> String {
    match board.turn {
        Color::White => format!("{}. {san}", board.fullmove_number),
        Color::Black if starts_line => format!("{}... {san}", board.fullmove_number),
        Color::Black => san.to_string(),
    }
}

/// Plays SAN moves from `root`, returning them as moves, or the first one that is not legal.
pub fn parse_san_line(root: &Board, sans: &[&str]) -> Result<Vec<Move>, String> {
    let mut board = root.without_eval_state();
    let mut moves = Vec::with_capacity(sans.len());
    for san in sans {
        let mut legal = MoveList::new();
        board.generate_legal_moves(&mut legal);
        let m = parse_typed_move(&board, legal.as_slice(), san)
            .ok_or_else(|| format!("'{san}' is not legal in {}", board.to_fen()))?;
        board.make_move(m);
        moves.push(m);
    }
    Ok(moves)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::START_FEN;

    #[test]
    fn typed_moves_accept_uci_and_san() {
        let board = Board::from_fen(START_FEN).unwrap();
        let mut legal = MoveList::new();
        board.generate_legal_moves(&mut legal);

        let uci = parse_typed_move(&board, legal.as_slice(), "g1f3").unwrap();
        let san = parse_typed_move(&board, legal.as_slice(), "Nf3").unwrap();

        assert_eq!(uci, san);
        assert!(parse_typed_move(&board, legal.as_slice(), "Nf4").is_none());
        assert!(parse_typed_move(&board, legal.as_slice(), "zz").is_none());
    }

    #[test]
    fn castling_can_be_typed_with_zeros_or_letters() {
        let board = Board::from_fen("r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1").unwrap();
        let mut legal = MoveList::new();
        board.generate_legal_moves(&mut legal);

        let letters = parse_typed_move(&board, legal.as_slice(), "O-O").unwrap();
        let zeros = parse_typed_move(&board, legal.as_slice(), "0-0").unwrap();

        assert!(letters.castle && letters == zeros);
    }

    #[test]
    fn over_specified_san_still_reads() {
        let board = Board::from_fen("rnbqkbnr/ppp1pppp/8/3p4/3P4/8/PPP1PPPP/RNBQKBNR b KQkq - 0 2").unwrap();
        let mut legal = MoveList::new();
        board.generate_legal_moves(&mut legal);

        let knight = parse_typed_move(&board, legal.as_slice(), "Nbd7").unwrap();

        assert_eq!(board.to_san(knight, legal.as_slice()), "Nd7");
    }

    #[test]
    fn ambiguous_san_reads_as_nothing() {
        let board = Board::from_fen("rnbqkb1r/ppp1pppp/5n2/3p4/3P4/8/PPP1PPPP/RNBQKBNR b KQkq - 0 2").unwrap();
        let mut legal = MoveList::new();
        board.generate_legal_moves(&mut legal);

        assert!(parse_typed_move(&board, legal.as_slice(), "Nd7").is_none());
    }

    #[test]
    fn pv_is_numbered_from_the_side_to_move() {
        let board = Board::from_fen("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1").unwrap();
        let line = parse_san_line(&board, &["e5", "Nf3"]).unwrap();

        assert_eq!(line_in_san(&board, &line), "1... e5 2. Nf3");
    }

    #[test]
    fn a_san_line_stops_at_the_first_illegal_move() {
        let board = Board::from_fen(START_FEN).unwrap();

        let error = parse_san_line(&board, &["d4", "d5", "Ke3"]).unwrap_err();

        assert!(error.starts_with("'Ke3' is not legal"), "{error}");
    }
}
