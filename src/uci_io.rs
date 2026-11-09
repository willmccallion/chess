use crate::board::Board;
use crate::types::{Move, MoveList, PieceKind};

pub fn parse_uci_move(b: &mut Board, s: &str) -> Option<Move> {
    let bytes = s.as_bytes();
    if bytes.len() < 4 {
        return None;
    }

    let from = square_index(bytes[0], bytes[1])?;
    let to = square_index(bytes[2], bytes[3])?;

    let promo = if bytes.len() >= 5 {
        match (bytes[4] as char).to_ascii_lowercase() {
            'q' => Some(PieceKind::Queen),
            'r' => Some(PieceKind::Rook),
            'b' => Some(PieceKind::Bishop),
            'n' => Some(PieceKind::Knight),
            _ => None,
        }
    } else {
        None
    };

    let mut moves = MoveList::new();
    b.generate_legal_moves(&mut moves);
    moves
        .as_slice()
        .iter()
        .find(|m| m.from == from && m.to == to && m.promotion == promo)
        .copied()
}

/// The 0..64 index of a square written as a file letter and rank digit, like `e` `4`.
fn square_index(file: u8, rank: u8) -> Option<u8> {
    let file = file.to_ascii_lowercase().checked_sub(b'a')?;
    let rank = rank.checked_sub(b'1')?;
    (file <= 7 && rank <= 7).then(|| rank * 8 + file)
}

pub fn format_uci(m: Move) -> String {
    let ff = (m.from % 8) + b'a';
    let fr = (m.from / 8) + b'1';
    let tf = (m.to % 8) + b'a';
    let tr = (m.to / 8) + b'1';
    let mut s = format!("{}{}{}{}", ff as char, fr as char, tf as char, tr as char);

    if let Some(pk) = m.promotion {
        s.push(match pk {
            PieceKind::Queen => 'q',
            PieceKind::Rook => 'r',
            PieceKind::Bishop => 'b',
            PieceKind::Knight => 'n',
            _ => unreachable!("invalid promotion piece kind"),
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
}
