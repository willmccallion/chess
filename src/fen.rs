use crate::board::Board;
use crate::types::{BK_CASTLE, BQ_CASTLE, Color, NO_SQ, Piece, WK_CASTLE, WQ_CASTLE};

pub fn parse_fen(fen: &str) -> Result<Board, String> {
    let mut b = Board::empty();

    let mut parts = fen.split_whitespace();
    let placement = parts.next().ok_or("missing placement")?;
    let side = parts.next().ok_or("missing side")?;
    let castle = parts.next().ok_or("missing castling")?;
    let ep = parts.next().ok_or("missing en-passant")?;
    let halfmove = parts.next().unwrap_or("0");
    let fullmove = parts.next().unwrap_or("1");

    let mut rank = 7;
    let mut file = 0;
    for ch in placement.chars() {
        match ch {
            '/' => {
                rank -= 1;
                file = 0;
            }
            '1'..='8' => {
                file += (ch as u8 - b'0') as i32;
            }
            c if c.is_ascii_alphabetic() => {
                if !(0..8).contains(&file) || !(0..8).contains(&rank) {
                    return Err("bad board in FEN".into());
                }
                let sq = (rank * 8 + file) as usize;
                let piece = Piece::from(c);
                b.place_piece(piece, sq);
                file += 1;
            }
            _ => return Err("bad char in placement".into()),
        }
    }

    b.turn = match side {
        "w" => Color::White,
        "b" => Color::Black,
        _ => return Err("bad side".into()),
    };

    b.castle = 0;
    if castle != "-" {
        for c in castle.chars() {
            match c {
                'K' => b.castle |= WK_CASTLE,
                'Q' => b.castle |= WQ_CASTLE,
                'k' => b.castle |= BK_CASTLE,
                'q' => b.castle |= BQ_CASTLE,
                _ => return Err("bad castling".into()),
            }
        }
    }

    if ep == "-" {
        b.en_passant_sq = NO_SQ;
    } else {
        let bytes = ep.as_bytes();
        if bytes.len() != 2 {
            return Err("bad ep".into());
        }
        let f = (bytes[0] as char).to_ascii_lowercase() as u8 - b'a';
        let r = (bytes[1] as char) as u8 - b'1';

        if f > 7 || r > 7 {
            return Err("bad ep coord".into());
        }

        b.en_passant_sq = (r as i32) * 8 + (f as i32);
    }

    b.halfmove_clock = halfmove.parse().unwrap_or(0);
    b.fullmove_number = fullmove.parse().unwrap_or(1);

    b.rebuild_derived();
    validate_kings(&b)?;
    b.recompute_zobrist();
    b.history.push(b.zobrist);

    Ok(b)
}

/// Each side needs exactly one king, and the side that just moved cannot be in check.
fn validate_kings(b: &Board) -> Result<(), String> {
    for king in [Piece::WK, Piece::BK] {
        if b.piece_bb[king.index()].count_ones() != 1 {
            return Err(format!("FEN must have exactly one {king} king"));
        }
    }
    let waiting_side = b.turn.other();
    let waiting_king = b.king_square(waiting_side) as i32;
    if b.is_square_attacked(waiting_king, b.turn) {
        return Err("side not to move is in check".into());
    }
    Ok(())
}

pub fn to_fen(b: &Board) -> String {
    let mut s = String::new();

    for r in (0..8).rev() {
        let mut empty = 0;

        for f in 0..8 {
            let sq = (r * 8 + f) as usize;
            let p = b.piece_on[sq];

            if p.is_empty() {
                empty += 1;
            } else {
                if empty > 0 {
                    s.push(char::from(b'0' + empty as u8));
                    empty = 0;
                }
                s.push(format!("{p}").chars().next().unwrap());
            }
        }

        if empty > 0 {
            s.push(char::from(b'0' + empty as u8));
        }

        if r != 0 {
            s.push('/');
        }
    }

    s.push(' ');
    s.push(if b.turn == Color::White { 'w' } else { 'b' });
    s.push(' ');

    if b.castle == 0 {
        s.push('-');
    } else {
        if b.castle & WK_CASTLE != 0 {
            s.push('K');
        }

        if b.castle & WQ_CASTLE != 0 {
            s.push('Q');
        }

        if b.castle & BK_CASTLE != 0 {
            s.push('k');
        }

        if b.castle & BQ_CASTLE != 0 {
            s.push('q');
        }
    }

    s.push(' ');
    if b.en_passant_sq == NO_SQ {
        s.push('-');
    } else {
        let f = (b.en_passant_sq % 8) as u8 + b'a';
        let r = (b.en_passant_sq / 8) as u8 + b'1';
        s.push(f as char);
        s.push(r as char);
    }

    s.push(' ');
    s.push_str(&b.halfmove_clock.to_string());
    s.push(' ');
    s.push_str(&b.fullmove_number.to_string());
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_position_where_side_not_to_move_is_in_check() {
        let fen = "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 b kq - 0 1";
        assert!(parse_fen(fen).is_err());
    }

    #[test]
    fn rejects_missing_or_extra_kings() {
        assert!(parse_fen("8/8/8/8/8/8/8/K7 w - - 0 1").is_err());
        assert!(parse_fen("k7/8/8/8/8/8/8/KK6 w - - 0 1").is_err());
    }

    #[test]
    fn round_trips_a_legal_position() {
        let fen = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";
        assert_eq!(to_fen(&parse_fen(fen).unwrap()), fen);
    }
}
