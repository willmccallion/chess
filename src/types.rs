use std::fmt;

pub type Bitboard = u64;
pub type ZKey = u64;

pub const NO_SQ: i32 = -1;

pub const START_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

// NNUE Constants
pub const NNUE_FEATURE_SIZE: usize = 256;

#[derive(Copy, Clone)]
#[repr(C, align(64))]
pub struct Accumulator {
    pub white: [i16; NNUE_FEATURE_SIZE],
    pub black: [i16; NNUE_FEATURE_SIZE],
}

impl Default for Accumulator {
    fn default() -> Self {
        Self {
            white: [0; NNUE_FEATURE_SIZE],
            black: [0; NNUE_FEATURE_SIZE],
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct UpdateBody {
    pub piece: Piece,
    pub sq: usize,
    pub add: bool,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Color {
    White = 0,
    Black = 1,
}

impl Color {
    #[inline(always)]
    pub fn other(self) -> Color {
        if self == Color::White {
            Color::Black
        } else {
            Color::White
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug)]
pub enum PieceKind {
    Pawn,
    Knight,
    Bishop,
    Rook,
    Queen,
    King,
}

impl PieceKind {
    pub fn to_char_upper(&self) -> char {
        match self {
            PieceKind::Pawn => 'P',
            PieceKind::Knight => 'N',
            PieceKind::Bishop => 'B',
            PieceKind::Rook => 'R',
            PieceKind::Queen => 'Q',
            PieceKind::King => 'K',
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Piece {
    Empty,
    WP,
    WN,
    WB,
    WR,
    WQ,
    WK,
    BP,
    BN,
    BB,
    BR,
    BQ,
    BK,
}

impl Piece {
    #[inline(always)]
    pub fn is_empty(self) -> bool {
        matches!(self, Piece::Empty)
    }

    #[inline(always)]
    pub fn color(self) -> Option<Color> {
        match self {
            Piece::WP | Piece::WN | Piece::WB | Piece::WR | Piece::WQ | Piece::WK => {
                Some(Color::White)
            }
            Piece::BP | Piece::BN | Piece::BB | Piece::BR | Piece::BQ | Piece::BK => {
                Some(Color::Black)
            }
            _ => None,
        }
    }

    #[inline(always)]
    pub fn kind(self) -> Option<PieceKind> {
        match self {
            Piece::WP | Piece::BP => Some(PieceKind::Pawn),
            Piece::WN | Piece::BN => Some(PieceKind::Knight),
            Piece::WB | Piece::BB => Some(PieceKind::Bishop),
            Piece::WR | Piece::BR => Some(PieceKind::Rook),
            Piece::WQ | Piece::BQ => Some(PieceKind::Queen),
            Piece::WK | Piece::BK => Some(PieceKind::King),
            _ => None,
        }
    }

    #[inline(always)]
    pub fn from_kind(kind: PieceKind, color: Color) -> Self {
        match (kind, color) {
            (PieceKind::Pawn, Color::White) => Piece::WP,
            (PieceKind::Knight, Color::White) => Piece::WN,
            (PieceKind::Bishop, Color::White) => Piece::WB,
            (PieceKind::Rook, Color::White) => Piece::WR,
            (PieceKind::Queen, Color::White) => Piece::WQ,
            (PieceKind::King, Color::White) => Piece::WK,
            (PieceKind::Pawn, Color::Black) => Piece::BP,
            (PieceKind::Knight, Color::Black) => Piece::BN,
            (PieceKind::Bishop, Color::Black) => Piece::BB,
            (PieceKind::Rook, Color::Black) => Piece::BR,
            (PieceKind::Queen, Color::Black) => Piece::BQ,
            (PieceKind::King, Color::Black) => Piece::BK,
        }
    }

    #[inline(always)]
    pub fn index(self) -> usize {
        self as usize
    }
}

impl From<char> for Piece {
    fn from(c: char) -> Self {
        match c {
            'P' => Piece::WP,
            'N' => Piece::WN,
            'B' => Piece::WB,
            'R' => Piece::WR,
            'Q' => Piece::WQ,
            'K' => Piece::WK,
            'p' => Piece::BP,
            'n' => Piece::BN,
            'b' => Piece::BB,
            'r' => Piece::BR,
            'q' => Piece::BQ,
            'k' => Piece::BK,
            _ => Piece::Empty,
        }
    }
}

impl fmt::Display for Piece {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = match self {
            Piece::Empty => '.',
            Piece::WP => 'P',
            Piece::WN => 'N',
            Piece::WB => 'B',
            Piece::WR => 'R',
            Piece::WQ => 'Q',
            Piece::WK => 'K',
            Piece::BP => 'p',
            Piece::BN => 'n',
            Piece::BB => 'b',
            Piece::BR => 'r',
            Piece::BQ => 'q',
            Piece::BK => 'k',
        };
        write!(f, "{c}")
    }
}

const MOVE_FLAG_DOUBLE_PUSH: u16 = 0b0001;
const MOVE_FLAG_KING_CASTLE: u16 = 0b0010;
const MOVE_FLAG_QUEEN_CASTLE: u16 = 0b0011;
const MOVE_FLAG_CAPTURE: u16 = 0b0100;
const MOVE_FLAG_EN_PASSANT: u16 = 0b0101;
const MOVE_FLAG_PROMOTION: u16 = 0b1000;
const MOVE_FLAG_PROMO_CAPTURE: u16 = 0b1100;

#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub struct Move {
    pub from: u8,
    pub to: u8,
    pub capture: bool,
    pub en_passant: bool,
    pub double_push: bool,
    pub castle: bool,
    pub promotion: Option<PieceKind>,
}

impl Move {
    #[inline(always)]
    pub fn quiet(from: u8, to: u8) -> Self {
        Self {
            from,
            to,
            capture: false,
            en_passant: false,
            double_push: false,
            castle: false,
            promotion: None,
        }
    }
}

impl From<Move> for u16 {
    #[inline(always)]
    fn from(m: Move) -> Self {
        let from_u16 = m.from as u16;
        let to_u16 = (m.to as u16) << 6;
        let mut flags = 0u16;

        if let Some(pk) = m.promotion {
            flags = if m.capture {
                MOVE_FLAG_PROMO_CAPTURE
            } else {
                MOVE_FLAG_PROMOTION
            };
            flags |= match pk {
                PieceKind::Knight => 0,
                PieceKind::Bishop => 1,
                PieceKind::Rook => 2,
                PieceKind::Queen => 3,
                _ => 0,
            };
        } else if m.en_passant {
            flags = MOVE_FLAG_EN_PASSANT;
        } else if m.capture {
            flags = MOVE_FLAG_CAPTURE;
        } else if m.castle {
            flags = if file_of(m.to as i32) > file_of(m.from as i32) {
                MOVE_FLAG_KING_CASTLE
            } else {
                MOVE_FLAG_QUEEN_CASTLE
            };
        } else if m.double_push {
            flags = MOVE_FLAG_DOUBLE_PUSH;
        }

        from_u16 | to_u16 | (flags << 12)
    }
}

impl From<u16> for Move {
    #[inline(always)]
    fn from(m: u16) -> Self {
        if m == 0 {
            return Move::default();
        }

        let from = (m & 0x3F) as u8;
        let to = ((m >> 6) & 0x3F) as u8;
        let flags = m >> 12;

        let mut mov = Move::quiet(from, to);

        if flags >= MOVE_FLAG_PROMOTION {
            mov.promotion = Some(match flags & 0b11 {
                0 => PieceKind::Knight,
                1 => PieceKind::Bishop,
                2 => PieceKind::Rook,
                _ => PieceKind::Queen,
            });
            if (flags & 0b0100) != 0 {
                mov.capture = true;
            }
        } else if flags == MOVE_FLAG_EN_PASSANT {
            mov.capture = true;
            mov.en_passant = true;
        } else if flags == MOVE_FLAG_CAPTURE {
            mov.capture = true;
        } else if flags == MOVE_FLAG_KING_CASTLE || flags == MOVE_FLAG_QUEEN_CASTLE {
            mov.castle = true;
        } else if flags == MOVE_FLAG_DOUBLE_PUSH {
            mov.double_push = true;
        }
        mov
    }
}

const MAX_MOVES: usize = 300;

#[derive(Clone, Copy)]
pub struct MoveList {
    pub moves: [Move; MAX_MOVES],
    pub len: usize,
}

impl Default for MoveList {
    fn default() -> Self {
        Self {
            moves: [Move::default(); MAX_MOVES],
            len: 0,
        }
    }
}

impl MoveList {
    #[inline(always)]
    pub fn new() -> Self {
        Self::default()
    }

    #[inline(always)]
    pub fn push(&mut self, m: Move) {
        self.moves[self.len] = m;
        self.len += 1;
    }

    #[inline(always)]
    pub fn clear(&mut self) {
        self.len = 0;
    }

    #[inline(always)]
    pub fn as_slice(&self) -> &[Move] {
        &self.moves[..self.len]
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline(always)]
    pub fn swap(&mut self, a: usize, b: usize) {
        self.moves.swap(a, b);
    }
}

impl<'a> IntoIterator for &'a MoveList {
    type Item = &'a Move;
    type IntoIter = std::slice::Iter<'a, Move>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

#[derive(Clone, Copy)]
pub struct Undo {
    pub captured_piece: Piece,
    pub old_castle: u8,
    pub old_en_passant_sq: i32,
    pub old_halfmove_clock: i32,
}

pub const WK_CASTLE: u8 = 1 << 0;
pub const WQ_CASTLE: u8 = 1 << 1;
pub const BK_CASTLE: u8 = 1 << 2;
pub const BQ_CASTLE: u8 = 1 << 3;

#[inline(always)]
pub fn file_of(sq: i32) -> i32 {
    sq & 7
}
#[inline(always)]
pub fn rank_of(sq: i32) -> i32 {
    sq >> 3
}
#[inline(always)]
pub fn in_board(sq: i32) -> bool {
    (0..64).contains(&sq)
}
#[inline(always)]
pub fn sq_to_str(sq: usize) -> String {
    let f = (sq % 8) as u8;
    let r = (sq / 8) as u8;
    format!("{}{}", (b'a' + f) as char, (b'1' + r) as char)
}
#[inline(always)]
pub fn file_char(sq: usize) -> char {
    ((sq % 8) as u8 + b'a') as char
}
#[inline(always)]
pub fn rank_char(sq: usize) -> char {
    ((sq / 8) as u8 + b'1') as char
}
