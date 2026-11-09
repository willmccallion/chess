use std::fmt;
use std::mem::MaybeUninit;

pub type Bitboard = u64;
pub type ZKey = u64;

/// A square of the board, numbered from a1 = 0 along each rank to h8 = 63.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default)]
#[repr(transparent)]
pub struct Square(u8);

impl Square {
    /// The square numbered `index`; numbers past 63 wrap around.
    #[inline(always)]
    pub const fn new(index: u8) -> Self {
        Self(index & 63)
    }

    /// The square on `file` (0 is the a-file) and `rank` (0 is the first).
    #[inline(always)]
    pub const fn from_file_rank(file: u8, rank: u8) -> Self {
        Self::new((rank & 7) * 8 + (file & 7))
    }

    /// The lowest-numbered square in `bb`, if it holds any.
    #[inline(always)]
    pub const fn first_in(bb: Bitboard) -> Option<Self> {
        if bb == 0 {
            None
        } else {
            Some(Self(bb.trailing_zeros() as u8))
        }
    }

    /// The square's number, for indexing per-square tables.
    #[inline(always)]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// The square's file, 0 for the a-file.
    #[inline(always)]
    pub const fn file(self) -> u8 {
        self.0 & 7
    }

    /// The square's rank, 0 for the first.
    #[inline(always)]
    pub const fn rank(self) -> u8 {
        self.0 >> 3
    }

    /// A bitboard holding only this square.
    #[inline(always)]
    pub const fn bb(self) -> Bitboard {
        1 << self.0
    }

    /// The square `delta` numbers away, if that is on the board. Moving
    /// off one side of a rank lands on the other side of the next.
    #[inline(always)]
    pub const fn offset(self, delta: i8) -> Option<Self> {
        let index = self.0 as i16 + delta as i16;
        if 0 <= index && index < 64 {
            Some(Self(index as u8))
        } else {
            None
        }
    }

    /// The square written as a file letter and rank digit, like `e4`, in
    /// either case.
    pub fn parse(text: &str) -> Option<Self> {
        let &[file, rank] = text.as_bytes() else {
            return None;
        };
        let file = file.to_ascii_lowercase().checked_sub(b'a')?;
        let rank = rank.checked_sub(b'1')?;
        (file < 8 && rank < 8).then(|| Self::from_file_rank(file, rank))
    }

    /// The square's file letter.
    pub const fn file_char(self) -> char {
        (b'a' + self.file()) as char
    }

    /// The square's rank digit.
    pub const fn rank_char(self) -> char {
        (b'1' + self.rank()) as char
    }
}

impl From<Square> for u8 {
    fn from(square: Square) -> Self {
        square.0
    }
}

impl fmt::Display for Square {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.file_char(), self.rank_char())
    }
}

/// The squares in `bb`, lowest first.
#[inline(always)]
pub fn squares(mut bb: Bitboard) -> impl Iterator<Item = Square> {
    std::iter::from_fn(move || {
        let square = Square::first_in(bb)?;
        bb &= bb - 1;
        Some(square)
    })
}

pub const START_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

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
    pub const fn from_kind(kind: PieceKind, color: Color) -> Self {
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
    pub const fn index(self) -> usize {
        self as usize
    }
}

impl Piece {
    /// The piece a FEN placement letter names, if it names one.
    pub const fn from_fen_char(c: char) -> Option<Self> {
        Some(match c {
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
            _ => return None,
        })
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
    pub from: Square,
    pub to: Square,
    pub capture: bool,
    pub en_passant: bool,
    pub double_push: bool,
    pub castle: bool,
    pub promotion: Option<PieceKind>,
}

impl Move {
    /// The square of the piece this move takes when `mover` plays it:
    /// beside the destination for en passant, the destination otherwise.
    #[inline(always)]
    pub const fn captured_square(self, mover: Color) -> usize {
        let to = self.to.index();
        match (self.en_passant, mover) {
            (true, Color::White) => to - 8,
            (true, Color::Black) => to + 8,
            (false, _) => to,
        }
    }

    /// Neither a capture nor a promotion.
    #[inline(always)]
    pub const fn is_quiet(self) -> bool {
        !self.capture && self.promotion.is_none()
    }

    #[inline(always)]
    pub const fn quiet(from: Square, to: Square) -> Self {
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

    /// A capture of the piece on `to`.
    #[inline(always)]
    pub const fn capture(from: Square, to: Square) -> Self {
        Self {
            capture: true,
            ..Self::quiet(from, to)
        }
    }

    /// A pawn's two-square advance.
    #[inline(always)]
    pub const fn double_push(from: Square, to: Square) -> Self {
        Self {
            double_push: true,
            ..Self::quiet(from, to)
        }
    }

    /// A pawn's advance to the last rank, promoting to `kind`.
    #[inline(always)]
    pub const fn promotion(from: Square, to: Square, kind: PieceKind) -> Self {
        Self {
            promotion: Some(kind),
            ..Self::quiet(from, to)
        }
    }

    /// A pawn's capture onto the last rank, promoting to `kind`.
    #[inline(always)]
    pub const fn promotion_capture(from: Square, to: Square, kind: PieceKind) -> Self {
        Self {
            promotion: Some(kind),
            ..Self::capture(from, to)
        }
    }

    /// A pawn's en-passant capture onto `to`.
    #[inline(always)]
    pub const fn en_passant(from: Square, to: Square) -> Self {
        Self {
            en_passant: true,
            ..Self::capture(from, to)
        }
    }

    /// The king's castling move.
    #[inline(always)]
    pub const fn castling(from: Square, to: Square) -> Self {
        Self {
            castle: true,
            ..Self::quiet(from, to)
        }
    }
}

/// The pieces a pawn promotes to, in the order the generators list them.
pub const PROMOTION_KINDS: [PieceKind; 4] = [
    PieceKind::Queen,
    PieceKind::Rook,
    PieceKind::Bishop,
    PieceKind::Knight,
];

impl From<Move> for u16 {
    #[inline(always)]
    fn from(m: Move) -> Self {
        let from_u16 = m.from.index() as u16;
        let to_u16 = (m.to.index() as u16) << 6;
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
            flags = if m.to.file() > m.from.file() {
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

        let from = Square::new((m & 0x3F) as u8);
        let to = Square::new(((m >> 6) & 0x3F) as u8);
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

pub const MAX_MOVES: usize = 300;

/// Fixed-capacity vector that does not initialize unused slots.
#[derive(Clone, Copy)]
pub struct StackVec<T: Copy, const N: usize> {
    items: [MaybeUninit<T>; N],
    len: usize,
}

impl<T: Copy, const N: usize> Default for StackVec<T, N> {
    #[inline(always)]
    fn default() -> Self {
        Self {
            items: [const { MaybeUninit::uninit() }; N],
            len: 0,
        }
    }
}

impl<T: Copy, const N: usize> StackVec<T, N> {
    /// Panics if the capacity `N` is exceeded.
    #[inline(always)]
    pub fn push(&mut self, item: T) {
        self.items[self.len] = MaybeUninit::new(item);
        self.len += 1;
    }

    #[inline(always)]
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Keeps the first `len` items; a larger `len` changes nothing.
    #[inline(always)]
    pub fn truncate(&mut self, len: usize) {
        self.len = self.len.min(len);
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
    pub fn as_slice(&self) -> &[T] {
        // SAFETY: slots 0..len were written by `push` and len <= N.
        unsafe { std::slice::from_raw_parts(self.items.as_ptr().cast::<T>(), self.len) }
    }

    #[inline(always)]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: slots 0..len were written by `push` and len <= N.
        unsafe { std::slice::from_raw_parts_mut(self.items.as_mut_ptr().cast::<T>(), self.len) }
    }
}

#[derive(Clone, Copy, Default)]
pub struct MoveList {
    moves: StackVec<Move, MAX_MOVES>,
}

impl MoveList {
    #[inline(always)]
    pub fn new() -> Self {
        Self::default()
    }

    #[inline(always)]
    pub fn push(&mut self, m: Move) {
        self.moves.push(m);
    }

    #[inline(always)]
    pub fn clear(&mut self) {
        self.moves.clear();
    }

    #[inline(always)]
    pub fn as_slice(&self) -> &[Move] {
        self.moves.as_slice()
    }

    #[inline(always)]
    pub fn get(&self, index: usize) -> Move {
        self.moves.as_slice()[index]
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.moves.len()
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.moves.is_empty()
    }

    #[inline(always)]
    pub fn swap(&mut self, a: usize, b: usize) {
        self.moves.as_mut_slice().swap(a, b);
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
    pub old_en_passant: Option<Square>,
    pub old_halfmove_clock: i32,
    pub old_plies_from_null: u32,
    pub old_zobrist: ZKey,
}

/// Where the rook moves from and to when the king castles from
/// `king_from` to `king_to`.
#[inline(always)]
pub const fn castling_rook_squares(king_from: usize, king_to: usize) -> (usize, usize) {
    if king_to > king_from {
        (king_to + 1, king_to - 1)
    } else {
        (king_to - 2, king_to + 1)
    }
}

/// One of the four castlings: the right it needs and the squares it uses.
pub struct Castling {
    pub right: u8,
    pub king_from: Square,
    pub king_to: Square,
    pub rook: Piece,
    /// The squares between the king and the rook, which must be empty.
    pub between: Bitboard,
    /// The squares the king crosses and lands on, which must not be attacked.
    pub king_path: [Square; 2],
}

impl Castling {
    /// Where the castling rook starts.
    #[inline(always)]
    pub const fn rook_from(&self) -> usize {
        castling_rook_squares(self.king_from.index(), self.king_to.index()).0
    }
}

/// Each colour's castlings, king side first, indexed by `Color`.
pub const CASTLINGS: [[Castling; 2]; 2] = [
    [
        Castling {
            right: WK_CASTLE,
            king_from: Square::new(4),
            king_to: Square::new(6),
            rook: Piece::WR,
            between: (1 << 5) | (1 << 6),
            king_path: [Square::new(5), Square::new(6)],
        },
        Castling {
            right: WQ_CASTLE,
            king_from: Square::new(4),
            king_to: Square::new(2),
            rook: Piece::WR,
            between: (1 << 1) | (1 << 2) | (1 << 3),
            king_path: [Square::new(3), Square::new(2)],
        },
    ],
    [
        Castling {
            right: BK_CASTLE,
            king_from: Square::new(60),
            king_to: Square::new(62),
            rook: Piece::BR,
            between: (1 << 61) | (1 << 62),
            king_path: [Square::new(61), Square::new(62)],
        },
        Castling {
            right: BQ_CASTLE,
            king_from: Square::new(60),
            king_to: Square::new(58),
            rook: Piece::BR,
            between: (1 << 57) | (1 << 58) | (1 << 59),
            king_path: [Square::new(59), Square::new(58)],
        },
    ],
];

pub const WK_CASTLE: u8 = 1 << 0;
pub const WQ_CASTLE: u8 = 1 << 1;
pub const BK_CASTLE: u8 = 1 << 2;
pub const BQ_CASTLE: u8 = 1 << 3;
