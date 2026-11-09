use crate::board::Board;
use crate::magics;
use crate::types::{BK_CASTLE, BQ_CASTLE, Color, Piece, PieceKind, Square, WK_CASTLE, WQ_CASTLE};
use std::fmt;

/// Why a FEN describes no position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenError {
    /// A required field is absent; this names it.
    MissingField(&'static str),
    RankCount(usize),
    /// Ranks count from 1.
    RankTooLong {
        rank: usize,
    },
    RankTooShort {
        rank: usize,
        squares: usize,
    },
    PieceLetter(char),
    SideToMove(String),
    CastlingLetter(char),
    EnPassantSquare(String),
    MoveCounter(String),
    /// The side has no king, or more than one.
    KingCount(Color),
    /// The side that just moved left its king attacked.
    WaitingSideInCheck,
}

impl fmt::Display for FenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingField(field) => write!(f, "missing {field}"),
            Self::RankCount(count) => write!(f, "placement has {count} ranks, not 8"),
            Self::RankTooLong { rank } => write!(f, "rank {rank} has more than 8 squares"),
            Self::RankTooShort { rank, squares } => {
                write!(f, "rank {rank} has {squares} squares, not 8")
            }
            Self::PieceLetter(letter) => write!(f, "bad piece letter {letter:?}"),
            Self::SideToMove(side) => write!(f, "bad side to move {side:?}"),
            Self::CastlingLetter(letter) => write!(f, "bad castling letter {letter:?}"),
            Self::EnPassantSquare(square) => write!(f, "bad en-passant square {square:?}"),
            Self::MoveCounter(counter) => write!(f, "bad move counter {counter:?}"),
            Self::KingCount(Color::White) => write!(f, "FEN must have exactly one white king"),
            Self::KingCount(Color::Black) => write!(f, "FEN must have exactly one black king"),
            Self::WaitingSideInCheck => write!(f, "side not to move is in check"),
        }
    }
}

impl std::error::Error for FenError {}

pub fn parse_fen(fen: &str) -> Result<Board, FenError> {
    let mut b = Board::empty();

    let mut parts = fen.split_whitespace();
    let mut field = |name| parts.next().ok_or(FenError::MissingField(name));
    let placement = field("placement")?;
    let side = field("side to move")?;
    let castle = field("castling")?;
    let ep = field("en-passant square")?;
    let halfmove = parts.next();
    let fullmove = parts.next();

    place_pieces(&mut b, placement)?;

    b.turn = match side {
        "w" => Color::White,
        "b" => Color::Black,
        _ => return Err(FenError::SideToMove(side.to_string())),
    };

    b.castle = 0;
    if castle != "-" {
        for c in castle.chars() {
            match c {
                'K' => b.castle |= WK_CASTLE,
                'Q' => b.castle |= WQ_CASTLE,
                'k' => b.castle |= BK_CASTLE,
                'q' => b.castle |= BQ_CASTLE,
                _ => return Err(FenError::CastlingLetter(c)),
            }
        }
    }

    let ep_sq = if ep == "-" {
        None
    } else {
        Some(Square::parse(ep).ok_or_else(|| FenError::EnPassantSquare(ep.to_string()))?)
    };

    b.halfmove_clock = parse_counter(halfmove, 0)?;
    b.fullmove_number = parse_counter(fullmove, 1)?;

    b.rebuild_derived();
    validate_kings(&b)?;
    b.en_passant = ep_sq.filter(|&sq| is_capturable_en_passant(&b, sq));
    b.recompute_zobrist();
    b.history.push(b.zobrist);

    Ok(b)
}

/// A move counter, or `default` when the FEN leaves it out.
fn parse_counter<T: std::str::FromStr>(counter: Option<&str>, default: T) -> Result<T, FenError> {
    counter.map_or(Ok(default), |text| {
        text.parse()
            .map_err(|_| FenError::MoveCounter(text.to_string()))
    })
}

/// Places the pieces of a FEN placement field: eight ranks from the
/// eighth down, each of eight squares.
fn place_pieces(b: &mut Board, placement: &str) -> Result<(), FenError> {
    let ranks: Vec<&str> = placement.split('/').collect();
    if ranks.len() != 8 {
        return Err(FenError::RankCount(ranks.len()));
    }
    for (rank, text) in (0..8).rev().zip(ranks) {
        let mut file = 0;
        for c in text.chars() {
            if let Some(empty) = c.to_digit(10).filter(|n| (1..=8).contains(n)) {
                file += empty as usize;
                continue;
            }
            let piece = Piece::from_fen_char(c).ok_or(FenError::PieceLetter(c))?;
            if file >= 8 {
                return Err(FenError::RankTooLong { rank: rank + 1 });
            }
            b.place_piece(piece, rank * 8 + file);
            file += 1;
        }
        if file != 8 {
            return Err(FenError::RankTooShort {
                rank: rank + 1,
                squares: file,
            });
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
fn validate_kings(b: &Board) -> Result<(), FenError> {
    for color in [Color::White, Color::Black] {
        if b.pieces(PieceKind::King, color).count_ones() != 1 {
            return Err(FenError::KingCount(color));
        }
    }
    let waiting_side = b.turn.other();
    if let Some(waiting_king) = b.king_square(waiting_side)
        && b.is_square_attacked(waiting_king, b.turn)
    {
        return Err(FenError::WaitingSideInCheck);
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
        assert_eq!(parse_fen(fen).err(), Some(FenError::WaitingSideInCheck));
    }

    #[test]
    fn rejects_missing_or_extra_kings() {
        assert_eq!(
            parse_fen("8/8/8/8/8/8/8/K7 w - - 0 1").err(),
            Some(FenError::KingCount(Color::Black))
        );
        assert_eq!(
            parse_fen("k7/8/8/8/8/8/8/KK6 w - - 0 1").err(),
            Some(FenError::KingCount(Color::White))
        );
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

        assert!(matches!(parsed, Ok(Err(FenError::EnPassantSquare(_)))));
    }

    #[test]
    fn rejects_an_unknown_piece_letter() {
        assert_eq!(
            parse_fen("4k3/8/8/8/8/8/8/4KX2 w - - 0 1").err(),
            Some(FenError::PieceLetter('X'))
        );
    }

    #[test]
    fn rejects_a_placement_without_eight_ranks() {
        assert_eq!(
            parse_fen("4k3/8/8/8/8/8/4K3 w - - 0 1").err(),
            Some(FenError::RankCount(7))
        );
    }

    #[test]
    fn rejects_a_rank_without_eight_squares() {
        assert_eq!(
            parse_fen("4k3/8/8/8/7/8/8/4K3 w - - 0 1").err(),
            Some(FenError::RankTooShort {
                rank: 4,
                squares: 7
            })
        );
    }

    #[test]
    fn move_counters_default_when_left_out_and_must_read_as_numbers() {
        let short = parse_fen("4k3/8/8/8/8/8/8/4K3 w - -").unwrap();

        assert_eq!((short.halfmove_clock, short.fullmove_number), (0, 1));
        assert_eq!(
            parse_fen("4k3/8/8/8/8/8/8/4K3 w - - x 1").err(),
            Some(FenError::MoveCounter("x".to_string()))
        );
    }

    #[test]
    fn a_missing_field_is_named() {
        assert_eq!(
            parse_fen("4k3/8/8/8/8/8/8/4K3 w").err(),
            Some(FenError::MissingField("castling"))
        );
    }
}
