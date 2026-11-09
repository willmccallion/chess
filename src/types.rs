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

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Color {
    White = 0,
    Black = 1,
}

impl Color {
    #[inline(always)]
    pub fn other(self) -> Self {
        if self == Self::White {
            Self::Black
        } else {
            Self::White
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
    pub const fn to_char_upper(&self) -> char {
        match self {
            Self::Pawn => 'P',
            Self::Knight => 'N',
            Self::Bishop => 'B',
            Self::Rook => 'R',
            Self::Queen => 'Q',
            Self::King => 'K',
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
    pub const fn is_empty(self) -> bool {
        matches!(self, Self::Empty)
    }

    #[inline(always)]
    pub const fn color(self) -> Option<Color> {
        match self {
            Self::WP | Self::WN | Self::WB | Self::WR | Self::WQ | Self::WK => Some(Color::White),
            Self::BP | Self::BN | Self::BB | Self::BR | Self::BQ | Self::BK => Some(Color::Black),
            _ => None,
        }
    }

    #[inline(always)]
    pub const fn kind(self) -> Option<PieceKind> {
        match self {
            Self::WP | Self::BP => Some(PieceKind::Pawn),
            Self::WN | Self::BN => Some(PieceKind::Knight),
            Self::WB | Self::BB => Some(PieceKind::Bishop),
            Self::WR | Self::BR => Some(PieceKind::Rook),
            Self::WQ | Self::BQ => Some(PieceKind::Queen),
            Self::WK | Self::BK => Some(PieceKind::King),
            _ => None,
        }
    }

    #[inline(always)]
    pub const fn from_kind(kind: PieceKind, color: Color) -> Self {
        match (kind, color) {
            (PieceKind::Pawn, Color::White) => Self::WP,
            (PieceKind::Knight, Color::White) => Self::WN,
            (PieceKind::Bishop, Color::White) => Self::WB,
            (PieceKind::Rook, Color::White) => Self::WR,
            (PieceKind::Queen, Color::White) => Self::WQ,
            (PieceKind::King, Color::White) => Self::WK,
            (PieceKind::Pawn, Color::Black) => Self::BP,
            (PieceKind::Knight, Color::Black) => Self::BN,
            (PieceKind::Bishop, Color::Black) => Self::BB,
            (PieceKind::Rook, Color::Black) => Self::BR,
            (PieceKind::Queen, Color::Black) => Self::BQ,
            (PieceKind::King, Color::Black) => Self::BK,
        }
    }

    #[inline(always)]
    pub const fn index(self) -> usize {
        self as usize
    }
}

impl Piece {
    /// The piece a FEN placement letter names, if it names one.
    /// The piece's FEN letter, upper case for white; `.` for an empty square.
    pub const fn to_char(self) -> char {
        match self {
            Self::Empty => '.',
            Self::WP => 'P',
            Self::WN => 'N',
            Self::WB => 'B',
            Self::WR => 'R',
            Self::WQ => 'Q',
            Self::WK => 'K',
            Self::BP => 'p',
            Self::BN => 'n',
            Self::BB => 'b',
            Self::BR => 'r',
            Self::BQ => 'q',
            Self::BK => 'k',
        }
    }

    pub const fn from_fen_char(c: char) -> Option<Self> {
        Some(match c {
            'P' => Self::WP,
            'N' => Self::WN,
            'B' => Self::WB,
            'R' => Self::WR,
            'Q' => Self::WQ,
            'K' => Self::WK,
            'p' => Self::BP,
            'n' => Self::BN,
            'b' => Self::BB,
            'r' => Self::BR,
            'q' => Self::BQ,
            'k' => Self::BK,
            _ => return None,
        })
    }
}

impl fmt::Display for Piece {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_char())
    }
}

const MOVE_FLAG_DOUBLE_PUSH: u16 = 0b0001;
const MOVE_FLAG_KING_CASTLE: u16 = 0b0010;
const MOVE_FLAG_QUEEN_CASTLE: u16 = 0b0011;
const MOVE_FLAG_CAPTURE: u16 = 0b0100;
const MOVE_FLAG_EN_PASSANT: u16 = 0b0101;
const MOVE_FLAG_PROMOTION: u16 = 0b1000;
const MOVE_FLAG_PROMO_CAPTURE: u16 = 0b1100;

/// A piece a pawn can promote to.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum PromotionPiece {
    Knight,
    Bishop,
    Rook,
    Queen,
}

impl PromotionPiece {
    /// The kind of piece it is.
    #[inline(always)]
    pub const fn kind(self) -> PieceKind {
        match self {
            Self::Knight => PieceKind::Knight,
            Self::Bishop => PieceKind::Bishop,
            Self::Rook => PieceKind::Rook,
            Self::Queen => PieceKind::Queen,
        }
    }

    /// The piece of `kind`, if a pawn can promote to it.
    #[inline(always)]
    pub const fn of(kind: PieceKind) -> Option<Self> {
        match kind {
            PieceKind::Knight => Some(Self::Knight),
            PieceKind::Bishop => Some(Self::Bishop),
            PieceKind::Rook => Some(Self::Rook),
            PieceKind::Queen => Some(Self::Queen),
            PieceKind::Pawn | PieceKind::King => None,
        }
    }
}

/// The pieces a pawn promotes to, in the order the generators list them.
pub const PROMOTION_PIECES: [PromotionPiece; 4] = [
    PromotionPiece::Queen,
    PromotionPiece::Rook,
    PromotionPiece::Bishop,
    PromotionPiece::Knight,
];

/// What a move does besides taking a piece from one square to another.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub enum MoveKind {
    #[default]
    Quiet,
    /// A pawn's two-square advance.
    DoublePush,
    Capture,
    /// A pawn's capture of the pawn beside it that has just advanced two.
    EnPassant,
    /// The king's move of castling; the rook moves with it.
    Castle,
    Promotion(PromotionPiece),
    PromotionCapture(PromotionPiece),
}

#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub struct Move {
    pub from: Square,
    pub to: Square,
    pub kind: MoveKind,
}

impl Move {
    #[inline(always)]
    pub const fn quiet(from: Square, to: Square) -> Self {
        Self {
            from,
            to,
            kind: MoveKind::Quiet,
        }
    }

    /// A capture of the piece on `to`.
    #[inline(always)]
    pub const fn capture(from: Square, to: Square) -> Self {
        Self {
            from,
            to,
            kind: MoveKind::Capture,
        }
    }

    /// A pawn's two-square advance.
    #[inline(always)]
    pub const fn double_push(from: Square, to: Square) -> Self {
        Self {
            from,
            to,
            kind: MoveKind::DoublePush,
        }
    }

    /// A pawn's advance to the last rank, promoting to `piece`.
    #[inline(always)]
    pub const fn promotion_push(from: Square, to: Square, piece: PromotionPiece) -> Self {
        Self {
            from,
            to,
            kind: MoveKind::Promotion(piece),
        }
    }

    /// A pawn's capture onto the last rank, promoting to `piece`.
    #[inline(always)]
    pub const fn promotion_capture(from: Square, to: Square, piece: PromotionPiece) -> Self {
        Self {
            from,
            to,
            kind: MoveKind::PromotionCapture(piece),
        }
    }

    /// A pawn's en-passant capture onto `to`.
    #[inline(always)]
    pub const fn en_passant(from: Square, to: Square) -> Self {
        Self {
            from,
            to,
            kind: MoveKind::EnPassant,
        }
    }

    /// The king's castling move.
    #[inline(always)]
    pub const fn castling(from: Square, to: Square) -> Self {
        Self {
            from,
            to,
            kind: MoveKind::Castle,
        }
    }

    /// Whether the move takes a piece, en passant included.
    #[inline(always)]
    pub const fn is_capture(self) -> bool {
        matches!(
            self.kind,
            MoveKind::Capture | MoveKind::EnPassant | MoveKind::PromotionCapture(_)
        )
    }

    #[inline(always)]
    pub const fn is_en_passant(self) -> bool {
        matches!(self.kind, MoveKind::EnPassant)
    }

    #[inline(always)]
    pub const fn is_castle(self) -> bool {
        matches!(self.kind, MoveKind::Castle)
    }

    #[inline(always)]
    pub const fn is_double_push(self) -> bool {
        matches!(self.kind, MoveKind::DoublePush)
    }

    /// The piece the move promotes to, if it promotes.
    #[inline(always)]
    pub const fn promotion_piece(self) -> Option<PromotionPiece> {
        match self.kind {
            MoveKind::Promotion(piece) | MoveKind::PromotionCapture(piece) => Some(piece),
            _ => None,
        }
    }

    /// The kind of piece the move promotes to, if it promotes.
    #[inline(always)]
    pub const fn promotion(self) -> Option<PieceKind> {
        match self.promotion_piece() {
            Some(piece) => Some(piece.kind()),
            None => None,
        }
    }

    /// Neither a capture nor a promotion.
    #[inline(always)]
    pub const fn is_quiet(self) -> bool {
        matches!(
            self.kind,
            MoveKind::Quiet | MoveKind::DoublePush | MoveKind::Castle
        )
    }

    /// The square of the piece this move takes when `mover` plays it:
    /// beside the destination for en passant, the destination otherwise.
    #[inline(always)]
    pub const fn captured_square(self, mover: Color) -> usize {
        let to = self.to.index();
        match (self.kind, mover) {
            (MoveKind::EnPassant, Color::White) => to - 8,
            (MoveKind::EnPassant, Color::Black) => to + 8,
            _ => to,
        }
    }
}

impl From<Move> for u16 {
    #[inline(always)]
    fn from(m: Move) -> Self {
        let flags = match m.kind {
            MoveKind::Quiet => 0,
            MoveKind::DoublePush => MOVE_FLAG_DOUBLE_PUSH,
            MoveKind::Castle if m.to.file() > m.from.file() => MOVE_FLAG_KING_CASTLE,
            MoveKind::Castle => MOVE_FLAG_QUEEN_CASTLE,
            MoveKind::Capture => MOVE_FLAG_CAPTURE,
            MoveKind::EnPassant => MOVE_FLAG_EN_PASSANT,
            MoveKind::Promotion(piece) => MOVE_FLAG_PROMOTION | piece as Self,
            MoveKind::PromotionCapture(piece) => MOVE_FLAG_PROMO_CAPTURE | piece as Self,
        };
        m.from.index() as Self | (m.to.index() as Self) << 6 | flags << 12
    }
}

impl From<u16> for Move {
    #[inline(always)]
    fn from(m: u16) -> Self {
        if m == 0 {
            return Self::default();
        }
        let from = Square::new((m & 0x3F) as u8);
        let to = Square::new(((m >> 6) & 0x3F) as u8);
        let flags = m >> 12;
        let piece = match flags & 0b11 {
            0 => PromotionPiece::Knight,
            1 => PromotionPiece::Bishop,
            2 => PromotionPiece::Rook,
            _ => PromotionPiece::Queen,
        };
        let kind = match flags {
            MOVE_FLAG_DOUBLE_PUSH => MoveKind::DoublePush,
            MOVE_FLAG_KING_CASTLE | MOVE_FLAG_QUEEN_CASTLE => MoveKind::Castle,
            MOVE_FLAG_CAPTURE => MoveKind::Capture,
            MOVE_FLAG_EN_PASSANT => MoveKind::EnPassant,
            f if f >= MOVE_FLAG_PROMO_CAPTURE => MoveKind::PromotionCapture(piece),
            f if f >= MOVE_FLAG_PROMOTION => MoveKind::Promotion(piece),
            _ => MoveKind::Quiet,
        };
        Self { from, to, kind }
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
    pub const fn push(&mut self, item: T) {
        self.items[self.len] = MaybeUninit::new(item);
        self.len += 1;
    }

    #[inline(always)]
    pub const fn clear(&mut self) {
        self.len = 0;
    }

    /// Keeps the first `len` items; a larger `len` changes nothing.
    #[inline(always)]
    pub fn truncate(&mut self, len: usize) {
        self.len = self.len.min(len);
    }

    #[inline(always)]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[inline(always)]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline(always)]
    pub const fn as_slice(&self) -> &[T] {
        // SAFETY: slots 0..len were written by `push` and len <= N.
        unsafe { std::slice::from_raw_parts(self.items.as_ptr().cast::<T>(), self.len) }
    }

    #[inline(always)]
    pub const fn as_mut_slice(&mut self) -> &mut [T] {
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
    pub const fn push(&mut self, m: Move) {
        self.moves.push(m);
    }

    #[inline(always)]
    pub const fn clear(&mut self) {
        self.moves.clear();
    }

    #[inline(always)]
    pub const fn as_slice(&self) -> &[Move] {
        self.moves.as_slice()
    }

    #[inline(always)]
    pub const fn get(&self, index: usize) -> Move {
        self.moves.as_slice()[index]
    }

    #[inline(always)]
    pub const fn len(&self) -> usize {
        self.moves.len()
    }

    #[inline(always)]
    pub const fn is_empty(&self) -> bool {
        self.moves.is_empty()
    }

    #[inline(always)]
    pub const fn swap(&mut self, a: usize, b: usize) {
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
