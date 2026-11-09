use crate::board::Board;
use crate::magics;
use crate::types::{BK_CASTLE, BQ_CASTLE, Color, Piece, PieceKind, Square, WK_CASTLE, WQ_CASTLE};

pub fn parse_fen(fen: &str) -> Result<Board, String> {
    let mut b = Board::empty();

    let mut parts = fen.split_whitespace();
    let placement = parts.next().ok_or("missing placement")?;
    let side = parts.next().ok_or("missing side")?;
    let castle = parts.next().ok_or("missing castling")?;
    let ep = parts.next().ok_or("missing en-passant")?;
    let halfmove = parts.next().unwrap_or("0");
    let fullmove = parts.next().unwrap_or("1");

    place_pieces(&mut b, placement)?;

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

    let ep_sq = if ep == "-" {
        None
    } else {
        Some(Square::parse(ep).ok_or_else(|| format!("bad en-passant square {ep:?}"))?)
    };

    b.halfmove_clock = halfmove.parse().unwrap_or(0);
    b.fullmove_number = fullmove.parse().unwrap_or(1);

    b.rebuild_derived();
    validate_kings(&b)?;
    b.en_passant = ep_sq.filter(|&sq| is_capturable_en_passant(&b, sq));
    b.recompute_zobrist();
    b.history.push(b.zobrist);

    Ok(b)
}

/// Places the pieces of a FEN placement field: eight ranks from the
/// eighth down, each of eight squares.
fn place_pieces(b: &mut Board, placement: &str) -> Result<(), String> {
    let ranks: Vec<&str> = placement.split('/').collect();
    if ranks.len() != 8 {
        return Err(format!("placement has {} ranks, not 8", ranks.len()));
    }
    for (rank, text) in (0..8).rev().zip(ranks) {
        let mut file = 0;
        for c in text.chars() {
            if let Some(empty) = c.to_digit(10).filter(|n| (1..=8).contains(n)) {
                file += empty as usize;
                continue;
            }
            let piece = Piece::from_fen_char(c).ok_or_else(|| format!("bad piece letter {c:?}"))?;
            if file >= 8 {
                return Err(format!("rank {} has more than 8 squares", rank + 1));
            }
            b.place_piece(piece, rank * 8 + file);
            file += 1;
        }
        if file != 8 {
            return Err(format!("rank {} has {file} squares, not 8", rank + 1));
        }
    }
    Ok(())
}

/// Whether the side to move can capture en passant on `sq`: the enemy pawn
/// that just stepped two squares stands in front of it, it and the square
/// the pawn left are empty, and a pawn of the side to move attacks it.
/// Like Stockfish, any other en-passant square in a FEN is dropped.
fn is_capturable_en_passant(b: &Board, sq: Square) -> bool {
    let (ep_rank, toward_pusher, attackers) = match b.turn {
        Color::White => (5, -8, magics::BLACK_PAWN_ATTACKS[sq.index()]),
        Color::Black => (2, 8, magics::WHITE_PAWN_ATTACKS[sq.index()]),
    };
    if sq.rank() != ep_rank {
        return false;
    }
    let (Some(pushed_pawn), Some(origin)) = (sq.offset(toward_pusher), sq.offset(-toward_pusher))
    else {
        return false;
    };
    let mover_pawn = Piece::from_kind(PieceKind::Pawn, b.turn.other());
    b.piece_on[pushed_pawn.index()] == mover_pawn
        && b.piece_on[sq.index()].is_empty()
        && b.piece_on[origin.index()].is_empty()
        && attackers & b.pieces(PieceKind::Pawn, b.turn) != 0
}

/// Each side needs exactly one king, and the side that just moved cannot be in check.
fn validate_kings(b: &Board) -> Result<(), String> {
    for king in [Piece::WK, Piece::BK] {
        if b.piece_bb[king.index()].count_ones() != 1 {
            return Err(format!("FEN must have exactly one {king} king"));
        }
    }
    let waiting_side = b.turn.other();
    if let Some(waiting_king) = b.king_square(waiting_side)
        && b.is_square_attacked(waiting_king, b.turn)
    {
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
                s.push(p.to_char());
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
    match b.en_passant {
        Some(ep) => s.push_str(&ep.to_string()),
        None => s.push('-'),
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

    #[test]
    fn drops_an_en_passant_square_no_pawn_can_capture_on() {
        let board = parse_fen("4k3/8/8/3P4/8/8/8/4K3 w - e6 0 1").unwrap();

        assert_eq!(board.en_passant, None);
    }

    #[test]
    fn keeps_an_en_passant_square_a_pawn_can_capture_on() {
        let fen = "4k3/8/8/3Pp3/8/8/8/4K3 w - e6 0 1";

        assert_eq!(to_fen(&parse_fen(fen).unwrap()), fen);
    }

    #[test]
    fn rejects_an_en_passant_square_off_the_board_without_panicking() {
        let parsed = std::panic::catch_unwind(|| parse_fen("4k3/8/8/8/8/8/8/4K3 w - e0 0 1"));

        assert!(matches!(parsed, Ok(Err(_))));
    }

    #[test]
    fn rejects_an_unknown_piece_letter() {
        assert!(parse_fen("4k3/8/8/8/8/8/8/4KX2 w - - 0 1").is_err());
    }

    #[test]
    fn rejects_a_placement_without_eight_ranks() {
        assert!(parse_fen("4k3/8/8/8/8/8/4K3 w - - 0 1").is_err());
    }

    #[test]
    fn rejects_a_rank_without_eight_squares() {
        assert!(parse_fen("4k3/8/8/8/7/8/8/4K3 w - - 0 1").is_err());
    }
}
