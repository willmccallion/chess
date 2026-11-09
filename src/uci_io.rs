use crate::board::Board;
use crate::types::{Move, MoveList, PieceKind, PromotionPiece, Square};

pub fn parse_uci_move(b: &mut Board, s: &str) -> Option<Move> {
    let bytes = s.as_bytes();
    let promo = match bytes.len() {
        4 => None,
        5 => Some(promotion_kind(bytes[4])?),
        _ => return None,
    };
    let from = Square::parse(s.get(0..2)?)?;
    let to = Square::parse(s.get(2..4)?)?;

    let mut moves = MoveList::new();
    b.generate_legal_moves(&mut moves);
    moves
        .as_slice()
        .iter()
        .find(|m| m.from == from && m.to == to && m.promotion() == promo)
        .copied()
}

/// The piece a promotion letter names, in either case.
fn promotion_kind(letter: u8) -> Option<PieceKind> {
    match letter.to_ascii_lowercase() {
        b'q' => Some(PieceKind::Queen),
        b'r' => Some(PieceKind::Rook),
        b'b' => Some(PieceKind::Bishop),
        b'n' => Some(PieceKind::Knight),
        _ => None,
    }
}

pub fn format_uci(m: Move) -> String {
    let mut s = format!("{}{}", m.from, m.to);

    if let Some(piece) = m.promotion_piece() {
        s.push(match piece {
            PromotionPiece::Queen => 'q',
            PromotionPiece::Rook => 'r',
            PromotionPiece::Bishop => 'b',
            PromotionPiece::Knight => 'n',
        });
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::START_FEN;

    fn parsed(fen: &str, text: &str) -> Option<String> {
        let mut board = Board::from_fen(fen).unwrap();
        parse_uci_move(&mut board, text).map(format_uci)
    }

    #[test]
    fn a_move_is_read_in_either_case_and_written_in_lower_case() {
        assert_eq!(parsed(START_FEN, "E2E4"), Some("e2e4".to_string()));
    }

    #[test]
    fn short_text_and_squares_off_the_board_are_not_moves() {
        for text in ["", "e2e", "i2i4", "e9e4", "e0e1"] {
            assert_eq!(parsed(START_FEN, text), None, "{text}");
        }
    }

    #[test]
    fn a_promotion_needs_a_piece_letter_and_keeps_it() {
        let fen = "8/4P1k1/8/8/8/8/6K1/8 w - - 0 1";

        assert_eq!(parsed(fen, "e7e8"), None);
        assert_eq!(parsed(fen, "e7e8x"), None);
        assert_eq!(parsed(fen, "e7e8N"), Some("e7e8n".to_string()));
        assert_eq!(parsed(fen, "e7e8q"), Some("e7e8q".to_string()));
    }

    #[test]
    fn a_letter_after_a_move_that_is_not_a_promotion_makes_it_no_move() {
        assert_eq!(parsed(START_FEN, "g1f3q"), None);
        assert_eq!(parsed(START_FEN, "g1f3x"), None);
    }
}
