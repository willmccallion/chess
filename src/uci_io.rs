use crate::board::Board;
use crate::types::{Move, MoveList, PieceKind};

pub fn parse_uci_move(b: &mut Board, s: &str) -> Option<Move> {
    let bytes = s.as_bytes();
    if bytes.len() < 4 {
        return None;
    }

    let f_file = (bytes[0] as char).to_ascii_lowercase() as u8 - b'a';
    let f_rank = (bytes[1] as char) as u8 - b'1';
    let t_file = (bytes[2] as char).to_ascii_lowercase() as u8 - b'a';
    let t_rank = (bytes[3] as char) as u8 - b'1';
    if f_file > 7 || f_rank > 7 || t_file > 7 || t_rank > 7 {
        return None;
    }

    let from = f_rank * 8 + f_file;
    let to = t_rank * 8 + t_file;

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
