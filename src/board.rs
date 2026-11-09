use crate::eval::{EvalState, IncrementalState, PositionView};
use crate::fen;
use crate::magics;
use crate::types::*;
use crate::zobrist;

/// The squares a piece move may land on, and which of them hold enemy
/// pieces to capture.
#[derive(Clone, Copy)]
struct PieceTargets {
    targets: Bitboard,
    enemy: Bitboard,
}

/// Which of a position's pseudo-legal moves a generator adds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MoveKinds {
    All,
    /// Captures, en passant and promotions.
    Tactical,
    /// Every other move, castling included.
    Quiet,
}

#[derive(Clone)]
pub struct Board {
    pub piece_bb: [Bitboard; 13],
    pub piece_on: [Piece; 64],
    /// Each colour's pieces, indexed by `Color`.
    pub color_bb: [Bitboard; 2],
    pub all_pieces: Bitboard,
    pub turn: Color,
    pub castle: u8,
    /// The square a pawn may capture onto en passant.
    pub en_passant: Option<Square>,
    pub halfmove_clock: i32,
    pub fullmove_number: i32,
    /// Plies since the last null move; positions before it cannot repeat.
    pub plies_from_null: u32,
    pub history: Vec<ZKey>,
    pub zobrist: ZKey,
    eval_state: EvalState,
}

impl Board {
    pub fn empty() -> Self {
        Self {
            piece_bb: [0; 13],
            piece_on: [Piece::Empty; 64],
            color_bb: [0; 2],
            all_pieces: 0,
            turn: Color::White,
            castle: 0,
            en_passant: None,
            halfmove_clock: 0,
            fullmove_number: 1,
            plies_from_null: 0,
            history: Vec::with_capacity(128),
            zobrist: 0,
            eval_state: EvalState::for_active(),
        }
    }

    /// Hash of the pawn structure (both colors).
    #[inline(always)]
    pub fn pawn_key(&self) -> u64 {
        let white = self.piece_bb[Piece::WP.index()].wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let black = self.piece_bb[Piece::BP.index()].wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        let mixed = (white ^ black.rotate_left(31)) ^ ((white ^ black) >> 29);
        mixed.wrapping_mul(0x1656_67B1_9E37_79F9)
    }

    #[inline(always)]
    pub const fn king_square(&self, c: Color) -> Option<Square> {
        Square::first_in(self.pieces(PieceKind::King, c))
    }

    /// Whether the side to move may castle as `castling` describes, its
    /// king's own square aside: the right is kept, the rook is home, the
    /// squares between are empty and none the king crosses is attacked.
    #[inline]
    fn can_castle(&self, castling: &Castling) -> bool {
        (self.castle & castling.right) != 0
            && (self.all_pieces & castling.between) == 0
            && self.piece_on[castling.rook_from()] == castling.rook
            && !castling
                .king_path
                .iter()
                .any(|&sq| self.is_square_attacked(sq, self.turn.other()))
    }

    /// `color`'s pieces.
    #[inline(always)]
    pub const fn pieces_of(&self, color: Color) -> Bitboard {
        self.color_bb[color as usize]
    }

    /// `color`'s pieces of `kind`.
    #[inline(always)]
    pub const fn pieces(&self, kind: PieceKind, color: Color) -> Bitboard {
        self.piece_bb[Piece::from_kind(kind, color).index()]
    }

    /// Both colours' pieces of `kind`.
    #[inline(always)]
    pub const fn kind_bb(&self, kind: PieceKind) -> Bitboard {
        self.pieces(kind, Color::White) | self.pieces(kind, Color::Black)
    }

    /// Whether the side to move's king is attacked; a board without that
    /// king is not in check.
    #[inline(always)]
    pub fn in_check(&self) -> bool {
        self.king_square(self.turn)
            .is_some_and(|king| self.is_square_attacked(king, self.turn.other()))
    }

    /// The standard starting position.
    pub fn start() -> Self {
        const BACK_RANK: [PieceKind; 8] = [
            PieceKind::Rook,
            PieceKind::Knight,
            PieceKind::Bishop,
            PieceKind::Queen,
            PieceKind::King,
            PieceKind::Bishop,
            PieceKind::Knight,
            PieceKind::Rook,
        ];
        let mut board = Self::empty();
        for (file, kind) in BACK_RANK.into_iter().enumerate() {
            board.place_piece(Piece::from_kind(kind, Color::White), file);
            board.place_piece(Piece::from_kind(PieceKind::Pawn, Color::White), 8 + file);
            board.place_piece(Piece::from_kind(PieceKind::Pawn, Color::Black), 48 + file);
            board.place_piece(Piece::from_kind(kind, Color::Black), 56 + file);
        }
        board.castle = WK_CASTLE | WQ_CASTLE | BK_CASTLE | BQ_CASTLE;
        board.rebuild_derived();
        board.recompute_zobrist();
        board.history.push(board.zobrist);
        board
    }

    pub fn from_fen(fen_str: &str) -> Result<Self, fen::FenError> {
        let mut b = fen::parse_fen(fen_str)?;
        b.eval_state.reset();
        Ok(b)
    }

    #[inline]
    pub fn place_piece(&mut self, p: Piece, sq: usize) {
        self.piece_on[sq] = p;
    }

    #[inline]
    pub fn rebuild_derived(&mut self) {
        self.piece_bb = [0; 13];
        self.color_bb = [0; 2];

        for sq in 0..64 {
            let p = self.piece_on[sq];
            if !p.is_empty() {
                self.piece_bb[p.index()] |= 1u64 << sq;
                if let Some(color) = p.color() {
                    self.color_bb[color as usize] |= 1u64 << sq;
                }
            }
        }
        self.all_pieces = self.color_bb[0] | self.color_bb[1];
        self.eval_state.reset();
    }

    #[inline]
    pub fn recompute_zobrist(&mut self) {
        let mut h = 0u64;
        for sq in 0..64 {
            let p = self.piece_on[sq];
            if !p.is_empty() {
                h ^= zobrist::ZOB.piece_key(p, sq);
            }
        }
        h ^= zobrist::ZOB.castle[(self.castle & 0xF) as usize];
        if let Some(ep) = self.en_passant {
            h ^= zobrist::ZOB.ep_file[usize::from(ep.file())];
        }
        if self.turn == Color::Black {
            h ^= zobrist::ZOB.side;
        }
        self.zobrist = h;
    }

    /// Number of earlier occurrences of the current position since the last irreversible move.
    #[inline]
    pub fn count_repetitions(&self) -> usize {
        let current_key = self.zobrist;
        self.history
            .iter()
            .rev()
            .take(self.halfmove_clock as usize + 1)
            .skip(1)
            .filter(|&&key| key == current_key)
            .count()
    }

    /// Threefold repetition, for adjudicating games.
    pub fn is_draw_by_repetition(&self) -> bool {
        self.count_repetitions() >= 2
    }

    /// Neither side can ever mate: bare kings plus at most one minor piece, or only
    /// bishops that all stand on squares of one color.
    pub fn has_insufficient_material(&self) -> bool {
        const LIGHT_SQUARES: u64 = 0x55AA_55AA_55AA_55AA;
        let mating_material = self.kind_bb(PieceKind::Pawn)
            | self.kind_bb(PieceKind::Rook)
            | self.kind_bb(PieceKind::Queen);
        if mating_material != 0 {
            return false;
        }
        let knights = self.kind_bb(PieceKind::Knight);
        let bishops = self.kind_bb(PieceKind::Bishop);
        let minor_count = (knights | bishops).count_ones();
        let bishops_share_a_color = bishops & LIGHT_SQUARES == 0 || bishops & !LIGHT_SQUARES == 0;
        minor_count <= 1 || (knights == 0 && bishops_share_a_color)
    }

    /// Repetition draw inside search: any repeat of a position reached after the root,
    /// or a threefold repetition involving positions before the root.
    #[inline]
    pub fn is_repetition(&self, plies_from_root: usize) -> bool {
        let reachable = (self.halfmove_clock.max(0) as usize)
            .min(self.plies_from_null as usize)
            .min(self.history.len().saturating_sub(1));
        let current_key = self.zobrist;
        let last = self.history.len() - 1;
        let mut occurrences_before_root = 0;
        let mut distance = 4;
        while distance <= reachable {
            if self.history[last - distance] == current_key {
                if distance < plies_from_root {
                    return true;
                }
                occurrences_before_root += 1;
                if occurrences_before_root == 2 {
                    return true;
                }
            }
            distance += 2;
        }
        false
    }

    #[inline]
    pub fn is_square_attacked(&self, square: Square, by: Color) -> bool {
        let sq = square.index();
        let (pawn, knight, king, bishop_like, rook_like) = if by == Color::White {
            (
                Piece::WP,
                Piece::WN,
                Piece::WK,
                self.piece_bb[Piece::WB.index()] | self.piece_bb[Piece::WQ.index()],
                self.piece_bb[Piece::WR.index()] | self.piece_bb[Piece::WQ.index()],
            )
        } else {
            (
                Piece::BP,
                Piece::BN,
                Piece::BK,
                self.piece_bb[Piece::BB.index()] | self.piece_bb[Piece::BQ.index()],
                self.piece_bb[Piece::BR.index()] | self.piece_bb[Piece::BQ.index()],
            )
        };

        let pawn_attacks = if by == Color::White {
            magics::BLACK_PAWN_ATTACKS[sq]
        } else {
            magics::WHITE_PAWN_ATTACKS[sq]
        };

        if (pawn_attacks & self.piece_bb[pawn.index()]) != 0 {
            return true;
        }
        if (magics::knight_attacks_from(sq) & self.piece_bb[knight.index()]) != 0 {
            return true;
        }
        if (magics::king_attacks_from(sq) & self.piece_bb[king.index()]) != 0 {
            return true;
        }
        if (magics::get_bishop_attacks(sq, self.all_pieces) & bishop_like) != 0 {
            return true;
        }
        if (magics::get_rook_attacks(sq, self.all_pieces) & rook_like) != 0 {
            return true;
        }
        false
    }

    #[inline]
    pub fn generate_pseudo_legal_moves(&self, out: &mut MoveList) {
        out.clear();
        self.gen_pawn_moves(MoveKinds::All, out);
        self.gen_piece_moves(MoveKinds::All, out);
    }

    #[inline]
    /// Whether the side to move has any legal move.
    pub fn has_legal_move(&self) -> bool {
        let mut moves = MoveList::new();
        self.generate_legal_moves(&mut moves);
        !moves.is_empty()
    }

    pub fn generate_legal_moves(&self, out: &mut MoveList) {
        let mut pseudo = MoveList::new();
        self.generate_pseudo_legal_moves(&mut pseudo);
        out.clear();
        let Some(info) = self.legality_info() else {
            return;
        };
        for &m in pseudo.as_slice() {
            if self.is_legal(m, &info) {
                out.push(m);
            }
        }
    }

    /// Adds `kinds` of the side to move's pawn moves: for each pawn, its
    /// advance or promotions, its double advance, then its captures to
    /// either side, each followed by en passant onto that square.
    fn gen_pawn_moves(&self, kinds: MoveKinds, out: &mut MoveList) {
        let white = self.turn == Color::White;
        let enemy = self.pieces_of(self.turn.other());
        let forward: i8 = if white { 8 } else { -8 };
        let (start_rank, promo_rank) = if white { (1, 6) } else { (6, 1) };

        for from in squares(self.pieces(PieceKind::Pawn, self.turn)) {
            let promoting = from.rank() == promo_rank;
            if let Some(to) = from.offset(forward)
                && (self.all_pieces & to.bb()) == 0
            {
                if promoting {
                    if kinds != MoveKinds::Quiet {
                        for kind in PROMOTION_PIECES {
                            out.push(Move::promotion_push(from, to, kind));
                        }
                    }
                } else if kinds != MoveKinds::Tactical {
                    out.push(Move::quiet(from, to));
                    if from.rank() == start_rank
                        && let Some(to2) = to.offset(forward)
                        && (self.all_pieces & to2.bb()) == 0
                    {
                        out.push(Move::double_push(from, to2));
                    }
                }
            }

            if kinds == MoveKinds::Quiet {
                continue;
            }
            for side in [-1, 1] {
                if (side == -1 && from.file() == 0) || (side == 1 && from.file() == 7) {
                    continue;
                }
                let Some(cap) = from.offset(forward + side) else {
                    continue;
                };
                if (enemy & cap.bb()) != 0 {
                    if promoting {
                        for kind in PROMOTION_PIECES {
                            out.push(Move::promotion_capture(from, cap, kind));
                        }
                    } else {
                        out.push(Move::capture(from, cap));
                    }
                }
                if self.en_passant == Some(cap) {
                    out.push(Move::en_passant(from, cap));
                }
            }
        }
    }

    /// Adds `kinds` of the side to move's knight, king, bishop, rook and
    /// queen moves, in that order, castling after the king's own moves.
    fn gen_piece_moves(&self, kinds: MoveKinds, out: &mut MoveList) {
        let enemy = self.pieces_of(self.turn.other());
        let occ = self.all_pieces;
        let targets = match kinds {
            MoveKinds::All => !self.pieces_of(self.turn),
            MoveKinds::Tactical => enemy,
            MoveKinds::Quiet => !occ,
        };
        let to = PieceTargets { targets, enemy };

        self.push_piece_moves(out, PieceKind::Knight, to, magics::knight_attacks_from);
        self.push_piece_moves(out, PieceKind::King, to, magics::king_attacks_from);
        if kinds != MoveKinds::Tactical {
            self.gen_castling(out);
        }
        self.push_piece_moves(out, PieceKind::Bishop, to, |from| {
            magics::get_bishop_attacks(from, occ)
        });
        self.push_piece_moves(out, PieceKind::Rook, to, |from| {
            magics::get_rook_attacks(from, occ)
        });
        self.push_piece_moves(out, PieceKind::Queen, to, |from| {
            magics::get_rook_attacks(from, occ) | magics::get_bishop_attacks(from, occ)
        });
    }

    /// Adds a move for each square in `to.targets` that `attacks` reaches
    /// from each of the side to move's pieces of `kind`.
    #[inline(always)]
    fn push_piece_moves(
        &self,
        out: &mut MoveList,
        kind: PieceKind,
        to: PieceTargets,
        attacks: impl Fn(usize) -> Bitboard,
    ) {
        let mut pieces = self.pieces(kind, self.turn);
        while pieces != 0 {
            let from = pieces.trailing_zeros() as usize;
            pieces &= pieces - 1;
            let mut moves = attacks(from) & to.targets;
            while moves != 0 {
                let target = moves.trailing_zeros() as u8;
                moves &= moves - 1;
                out.push(if (to.enemy & (1u64 << target)) != 0 {
                    Move::capture(Square::new(from as u8), Square::new(target))
                } else {
                    Move::quiet(Square::new(from as u8), Square::new(target))
                });
            }
        }
    }

    /// Adds the castlings the side to move may make.
    fn gen_castling(&self, out: &mut MoveList) {
        let Some(king) = self.king_square(self.turn) else {
            return;
        };
        if self.is_square_attacked(king, self.turn.other()) {
            return;
        }
        for castling in &CASTLINGS[self.turn as usize] {
            if self.can_castle(castling) {
                out.push(Move::castling(castling.king_from, castling.king_to));
            }
        }
    }

    /// Generates captures, en passant and promotions: the moves the move
    /// picker scores as tactical.
    pub fn generate_captures(&self, out: &mut MoveList) {
        out.clear();
        self.gen_pawn_moves(MoveKinds::Tactical, out);
        self.gen_piece_moves(MoveKinds::Tactical, out);
    }

    /// Generates the moves `generate_captures` leaves out.
    pub fn generate_quiets(&self, out: &mut MoveList) {
        out.clear();
        self.gen_pawn_moves(MoveKinds::Quiet, out);
        self.gen_piece_moves(MoveKinds::Quiet, out);
    }

    /// Whether `m` could be played here if checks were ignored. Must match
    /// every assumption that `make_move` relies on.
    pub fn is_pseudo_legal(&self, m: Move) -> bool {
        if m.from == m.to {
            return false;
        }
        let Some(kind) = self.own_piece_on(m.from) else {
            return false;
        };
        if !Self::flags_fit_piece(m, kind) {
            return false;
        }
        if m.is_castle() {
            return self.can_castle_as(m);
        }
        self.target_fits(m) && self.piece_reaches(kind, m)
    }

    /// The kind of the side to move's piece on `square`, if it has one there.
    fn own_piece_on(&self, square: Square) -> Option<PieceKind> {
        let piece = self.piece_on[square.index()];
        if piece.color() != Some(self.turn) {
            return None;
        }
        piece.kind()
    }

    /// Whether only a king castles and only a pawn captures en passant,
    /// double-pushes or promotes.
    fn flags_fit_piece(m: Move, kind: PieceKind) -> bool {
        if m.is_castle() {
            return kind == PieceKind::King;
        }
        let pawn_only = m.is_en_passant() || m.is_double_push() || m.promotion().is_some();
        !pawn_only || kind == PieceKind::Pawn
    }

    /// Whether the king's move `m` is one of the side to move's castlings and
    /// may be played now. make_move moves the rook from the square the
    /// castling names, so every check the generator makes is repeated here.
    fn can_castle_as(&self, m: Move) -> bool {
        let Some(castling) = CASTLINGS[self.turn as usize]
            .iter()
            .find(|c| c.king_from == m.from && c.king_to == m.to)
        else {
            return false;
        };
        !self.is_square_attacked(m.from, self.turn.other()) && self.can_castle(castling)
    }

    /// Whether `m`'s target square holds what the move expects: nothing for a
    /// quiet move, an enemy piece for a capture, and for en passant an empty
    /// en-passant square beside an enemy pawn.
    fn target_fits(&self, m: Move) -> bool {
        let target = self.piece_on[m.to.index()];
        if m.is_en_passant() {
            let enemy_pawn = Piece::from_kind(PieceKind::Pawn, self.turn.other());
            return self.en_passant == Some(m.to)
                && target == Piece::Empty
                && self.piece_on[m.captured_square(self.turn)] == enemy_pawn;
        }
        if m.is_capture() {
            return target != Piece::Empty && target.color() != Some(self.turn);
        }
        target == Piece::Empty
    }

    /// Whether a `kind` on `m.from` can move to `m.to` on this board.
    fn piece_reaches(&self, kind: PieceKind, m: Move) -> bool {
        let (from, occupied) = (m.from.index(), self.all_pieces);
        let targets = match kind {
            PieceKind::Pawn => return self.pawn_reaches(m),
            PieceKind::Knight => magics::knight_attacks_from(from),
            PieceKind::Bishop => magics::get_bishop_attacks(from, occupied),
            PieceKind::Rook => magics::get_rook_attacks(from, occupied),
            PieceKind::Queen => {
                magics::get_bishop_attacks(from, occupied)
                    | magics::get_rook_attacks(from, occupied)
            }
            PieceKind::King => magics::king_attacks_from(from),
        };
        targets & m.to.bb() != 0
    }

    /// Whether the side to move's pawn on `m.from` makes `m`: a diagonal step
    /// for a capture, two clear squares from its start rank for a double
    /// push, else one step; promoting exactly when it reaches the last rank.
    fn pawn_reaches(&self, m: Move) -> bool {
        let white = self.turn == Color::White;
        let forward: i8 = if white { 8 } else { -8 };
        let (start_rank, promo_rank) = if white { (1, 6) } else { (6, 1) };
        let ahead = m.from.offset(forward);
        let reaches = if m.is_capture() {
            m.to.file().abs_diff(m.from.file()) == 1
                && (m.from.offset(forward - 1) == Some(m.to)
                    || m.from.offset(forward + 1) == Some(m.to))
        } else if m.is_double_push() {
            m.from.rank() == start_rank
                && ahead.is_some_and(|mid| {
                    (self.all_pieces & mid.bb()) == 0 && mid.offset(forward) == Some(m.to)
                })
        } else {
            ahead == Some(m.to)
        };
        reaches && (m.from.rank() == promo_rank) == m.promotion().is_some()
    }

    #[inline]
    pub fn make_move(&mut self, m: Move) -> Undo {
        let mut undo = Undo {
            captured_piece: Piece::Empty,
            old_castle: self.castle,
            old_en_passant: self.en_passant,
            old_halfmove_clock: self.halfmove_clock,
            old_plies_from_null: self.plies_from_null,
            old_zobrist: self.zobrist,
        };
        self.plies_from_null += 1;

        let from = m.from.index();
        let to = m.to.index();
        let moving = self.piece_on[from];

        let position = PositionView {
            piece_on: &self.piece_on,
            piece_bb: &self.piece_bb,
            occupied: self.all_pieces,
            turn: self.turn,
            halfmove_clock: self.halfmove_clock,
        };
        self.eval_state.push_move(position, m);

        if m.is_capture() {
            let cap_sq = m.captured_square(self.turn);
            undo.captured_piece = self.piece_on[cap_sq];
        }

        if let Some(ep) = self.en_passant.take() {
            self.zobrist ^= zobrist::ZOB.ep_file[usize::from(ep.file())];
        }

        self.zobrist ^= zobrist::ZOB.piece_key(moving, from);
        self.piece_on[from] = Piece::Empty;
        self.piece_bb[moving.index()] ^= 1u64 << from;

        self.color_bb[self.turn as usize] ^= 1u64 << from;

        if m.is_capture() {
            let cap_sq = m.captured_square(self.turn);

            let captured = undo.captured_piece;
            if !captured.is_empty() {
                self.zobrist ^= zobrist::ZOB.piece_key(captured, cap_sq);
                self.piece_on[cap_sq] = Piece::Empty;
                self.piece_bb[captured.index()] ^= 1u64 << cap_sq;
                self.color_bb[self.turn.other() as usize] ^= 1u64 << cap_sq;
            }
        }

        if let Some(pk) = m.promotion() {
            let promoted_piece = Piece::from_kind(pk, self.turn);
            self.piece_on[to] = promoted_piece;
            self.piece_bb[promoted_piece.index()] |= 1u64 << to;
            self.zobrist ^= zobrist::ZOB.piece_key(promoted_piece, to);
        } else {
            self.piece_on[to] = moving;
            self.piece_bb[moving.index()] |= 1u64 << to;
            self.zobrist ^= zobrist::ZOB.piece_key(moving, to);
        }

        self.color_bb[self.turn as usize] |= 1u64 << to;

        if m.is_castle() {
            let (rook_from, rook_to) = castling_rook_squares(from, to);
            let rook_piece = self.piece_on[rook_from];
            self.zobrist ^= zobrist::ZOB.piece_key(rook_piece, rook_from);
            self.zobrist ^= zobrist::ZOB.piece_key(rook_piece, rook_to);
            self.piece_on[rook_from] = Piece::Empty;
            self.piece_on[rook_to] = rook_piece;

            let rook_bb = (1u64 << rook_from) | (1u64 << rook_to);
            self.piece_bb[rook_piece.index()] ^= rook_bb;

            self.color_bb[self.turn as usize] ^= rook_bb;
        }

        if m.is_double_push() {
            let ep = Square::from_file_rank(m.from.file(), (m.from.rank() + m.to.rank()) / 2);
            self.en_passant = Some(ep);
            self.zobrist ^= zobrist::ZOB.ep_file[usize::from(ep.file())];
        }

        if matches!(moving.kind(), Some(PieceKind::Pawn)) || m.is_capture() {
            self.halfmove_clock = 0;
        } else {
            self.halfmove_clock += 1;
        }

        self.zobrist ^= zobrist::ZOB.castle[(self.castle & 0xF) as usize];
        match moving {
            Piece::WK => self.castle &= !(WK_CASTLE | WQ_CASTLE),
            Piece::BK => self.castle &= !(BK_CASTLE | BQ_CASTLE),
            _ => {}
        }
        match from {
            0 => self.castle &= !WQ_CASTLE,
            7 => self.castle &= !WK_CASTLE,
            56 => self.castle &= !BQ_CASTLE,
            63 => self.castle &= !BK_CASTLE,
            _ => {}
        }
        if m.is_capture() {
            match to {
                0 => self.castle &= !WQ_CASTLE,
                7 => self.castle &= !WK_CASTLE,
                56 => self.castle &= !BQ_CASTLE,
                63 => self.castle &= !BK_CASTLE,
                _ => {}
            }
        }
        self.zobrist ^= zobrist::ZOB.castle[(self.castle & 0xF) as usize];

        self.all_pieces = self.color_bb[0] | self.color_bb[1];
        self.zobrist ^= zobrist::ZOB.side;
        if self.turn == Color::Black {
            self.fullmove_number += 1;
        }

        self.turn = self.turn.other();
        self.history.push(self.zobrist);

        undo
    }

    pub fn position_view(&self) -> PositionView<'_> {
        PositionView {
            piece_on: &self.piece_on,
            piece_bb: &self.piece_bb,
            occupied: self.all_pieces,
            turn: self.turn,
            halfmove_clock: self.halfmove_clock,
        }
    }

    /// A copy without evaluation state, so it and its clones are cheap; evaluating it
    /// rebuilds the state on first use.
    pub fn without_eval_state(&self) -> Self {
        Self {
            piece_bb: self.piece_bb,
            piece_on: self.piece_on,
            color_bb: self.color_bb,
            all_pieces: self.all_pieces,
            turn: self.turn,
            castle: self.castle,
            en_passant: self.en_passant,
            halfmove_clock: self.halfmove_clock,
            fullmove_number: self.fullmove_number,
            plies_from_null: self.plies_from_null,
            history: self.history.clone(),
            zobrist: self.zobrist,
            eval_state: EvalState::Stateless,
        }
    }

    /// Drops the evaluation state of earlier positions; the current one is recomputed on demand.
    pub fn forget_eval_history(&mut self) {
        self.eval_state.reset();
    }

    /// The evaluation state and the position it describes, borrowed together.
    pub fn eval_state_and_view(&mut self) -> (&mut EvalState, PositionView<'_>) {
        let position = PositionView {
            piece_on: &self.piece_on,
            piece_bb: &self.piece_bb,
            occupied: self.all_pieces,
            turn: self.turn,
            halfmove_clock: self.halfmove_clock,
        };
        (&mut self.eval_state, position)
    }

    #[inline]
    pub fn unmake_move(&mut self, m: Move, undo: Undo) {
        let from = m.from.index();
        let to = m.to.index();

        let piece_on_to = self.piece_on[to];
        let moving_piece = if m.promotion().is_some() {
            Piece::from_kind(PieceKind::Pawn, self.turn.other())
        } else {
            piece_on_to
        };

        self.eval_state.pop_move();

        self.history.pop();
        self.zobrist = undo.old_zobrist;

        self.turn = self.turn.other();
        if self.turn == Color::Black {
            self.fullmove_number -= 1;
        }

        self.castle = undo.old_castle;
        self.en_passant = undo.old_en_passant;
        self.halfmove_clock = undo.old_halfmove_clock;
        self.plies_from_null = undo.old_plies_from_null;

        self.piece_on[from] = moving_piece;
        self.piece_bb[moving_piece.index()] |= 1u64 << from;
        self.color_bb[self.turn as usize] |= 1u64 << from;

        self.piece_bb[piece_on_to.index()] &= !(1u64 << to);
        self.color_bb[self.turn as usize] &= !(1u64 << to);

        if m.is_capture() {
            let captured = undo.captured_piece;

            if m.is_en_passant() {
                self.piece_on[to] = Piece::Empty;
            }
            let cap_sq = m.captured_square(self.turn);

            self.piece_on[cap_sq] = captured;
            if !captured.is_empty() {
                self.piece_bb[captured.index()] |= 1u64 << cap_sq;
                self.color_bb[self.turn.other() as usize] |= 1u64 << cap_sq;
            }
        } else {
            self.piece_on[to] = Piece::Empty;
        }

        if m.is_castle() {
            let (rook_from, rook_to) = castling_rook_squares(from, to);
            let rook = self.piece_on[rook_to];
            self.piece_on[rook_from] = rook;
            self.piece_on[rook_to] = Piece::Empty;

            let rook_bb = (1u64 << rook_from) | (1u64 << rook_to);
            self.piece_bb[rook.index()] ^= rook_bb;
            self.color_bb[self.turn as usize] ^= rook_bb;
        }

        self.all_pieces = self.color_bb[0] | self.color_bb[1];
    }

    #[inline]
    pub fn make_null_move(&mut self) -> Undo {
        let undo = Undo {
            captured_piece: Piece::Empty,
            old_castle: self.castle,
            old_en_passant: self.en_passant,
            old_halfmove_clock: self.halfmove_clock,
            old_plies_from_null: self.plies_from_null,
            old_zobrist: self.zobrist,
        };
        self.plies_from_null = 0;

        if let Some(ep) = self.en_passant.take() {
            self.zobrist ^= zobrist::ZOB.ep_file[usize::from(ep.file())];
        }

        self.turn = self.turn.other();
        self.zobrist ^= zobrist::ZOB.side;
        self.halfmove_clock += 1;
        self.history.push(self.zobrist);

        undo
    }

    #[inline]
    pub fn unmake_null_move(&mut self, undo: Undo) {
        self.history.pop();
        self.zobrist = undo.old_zobrist;
        self.turn = self.turn.other();
        self.en_passant = undo.old_en_passant;
        self.halfmove_clock = undo.old_halfmove_clock;
        self.plies_from_null = undo.old_plies_from_null;
    }

    pub fn to_fen(&self) -> String {
        fen::to_fen(self)
    }

    pub fn to_san(&self, m: Move, legal_moves: &[Move]) -> String {
        if m.is_castle() {
            return if m.to > m.from { "O-O" } else { "O-O-O" }.to_string();
        }

        let from = m.from.index();
        let moving_piece = self.piece_on[from];
        let mut san = String::new();

        if let Some(pk) = moving_piece.kind() {
            match pk {
                PieceKind::Pawn => {
                    if m.is_capture() {
                        san.push(m.from.file_char());
                    }
                }
                _ => {
                    san.push(pk.to_char_upper());
                    let mut ambiguous_moves = Vec::new();
                    for other_move in legal_moves {
                        let other_from = other_move.from.index();
                        if self.piece_on[other_from].kind() == Some(pk)
                            && other_from != from
                            && other_move.to == m.to
                        {
                            ambiguous_moves.push(other_move);
                        }
                    }

                    if !ambiguous_moves.is_empty() {
                        let mut file_is_unique = true;
                        let mut rank_is_unique = true;

                        for amb_move in &ambiguous_moves {
                            if amb_move.from.file() == m.from.file() {
                                file_is_unique = false;
                            }
                            if amb_move.from.rank() == m.from.rank() {
                                rank_is_unique = false;
                            }
                        }

                        if file_is_unique {
                            san.push(m.from.file_char());
                        } else if rank_is_unique {
                            san.push(m.from.rank_char());
                        } else {
                            san.push_str(&m.from.to_string());
                        }
                    }
                }
            }
        }

        if m.is_capture() {
            san.push('x');
        }

        san.push_str(&m.to.to_string());

        if let Some(promo) = m.promotion() {
            san.push('=');
            san.push(promo.to_char_upper());
        }

        let mut after = self.clone();
        after.make_move(m);
        if after.in_check() {
            san.push(if after.has_legal_move() { '+' } else { '#' });
        }
        san
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::random_game_positions;
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    #[test]
    fn the_start_position_is_the_one_its_fen_describes() {
        let parsed = Board::from_fen(START_FEN).unwrap();
        let start = Board::start();

        assert_eq!(start.to_fen(), START_FEN);
        assert_eq!(start.zobrist, parsed.zobrist);
        assert_eq!(start.history, parsed.history);
    }

    #[test]
    fn played_games_keep_the_same_legal_moves_as_a_board_read_from_their_fen() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..300 {
            let mut board = Board::from_fen(START_FEN).unwrap();
            for _ in 0..120 {
                let mut played = MoveList::new();
                board.generate_legal_moves(&mut played);
                let mut fresh_moves = MoveList::new();
                Board::from_fen(&board.to_fen())
                    .unwrap()
                    .generate_legal_moves(&mut fresh_moves);
                let mut ours: Vec<_> = played
                    .as_slice()
                    .iter()
                    .map(|m| (m.from, m.to, m.promotion()))
                    .collect();
                let mut fresh: Vec<_> = fresh_moves
                    .as_slice()
                    .iter()
                    .map(|m| (m.from, m.to, m.promotion()))
                    .collect();
                ours.sort_unstable_by_key(|&(f, t, p)| (f, t, p.map(|k| k as u8)));
                fresh.sort_unstable_by_key(|&(f, t, p)| (f, t, p.map(|k| k as u8)));
                assert_eq!(ours, fresh, "{}", board.to_fen());
                if played.is_empty() {
                    break;
                }
                let m = played.get(rng.gen_range(0..played.len()));
                board.make_move(m);
            }
        }
    }

    #[test]
    fn pseudo_legality_agrees_with_the_generator_for_moves_from_anywhere() {
        let positions = random_game_positions(11, 40, 80);
        let mut candidates: Vec<Move> = (0..=u16::MAX).map(Move::from).collect();
        for board in &positions {
            let mut moves = MoveList::new();
            board.generate_pseudo_legal_moves(&mut moves);
            candidates.extend_from_slice(moves.as_slice());
        }
        candidates.sort_unstable_by_key(|&m| u16::from(m));
        candidates.dedup();

        for board in positions.iter().step_by(40) {
            let mut generated = MoveList::new();
            board.generate_pseudo_legal_moves(&mut generated);
            for &m in &candidates {
                assert_eq!(
                    board.is_pseudo_legal(m),
                    generated.as_slice().contains(&m),
                    "{m:?} in {}",
                    board.to_fen()
                );
            }
        }
    }

    #[test]
    fn making_a_move_keeps_the_keys_incremental_and_unmaking_restores_them() {
        for before in random_game_positions(13, 60, 100) {
            let mut moves = MoveList::new();
            before.generate_legal_moves(&mut moves);
            for &m in moves.as_slice() {
                let mut board = before.clone();
                let undo = board.make_move(m);
                let mut recomputed = board.clone();
                recomputed.recompute_zobrist();
                assert_eq!(
                    board.zobrist,
                    recomputed.zobrist,
                    "{m:?} from {}",
                    before.to_fen()
                );
                assert_eq!(
                    board.all_pieces,
                    board.pieces_of(Color::White) | board.pieces_of(Color::Black)
                );

                board.unmake_move(m, undo);
                assert_eq!(
                    (board.zobrist, board.to_fen(), board.all_pieces),
                    (before.zobrist, before.to_fen(), before.all_pieces),
                    "{m:?} from {}",
                    before.to_fen()
                );
            }
        }
    }

    /// `text`, a UCI move legal in `fen`, in SAN.
    fn san(fen: &str, text: &str) -> String {
        let board = Board::from_fen(fen).unwrap();
        let m = crate::uci_io::parse_uci_move(&board, text).unwrap();
        let mut legal = MoveList::new();
        board.generate_legal_moves(&mut legal);
        board.to_san(m, legal.as_slice())
    }

    #[test]
    fn san_names_the_file_rank_or_square_that_tells_two_pieces_apart() {
        assert_eq!(san("4k3/8/8/8/8/8/8/1N1K1N2 w - - 0 1", "b1d2"), "Nbd2");
        assert_eq!(san("4k3/8/8/R7/8/8/8/R3K3 w - - 0 1", "a1a3"), "R1a3");
        assert_eq!(san("4k3/8/8/8/8/Q7/8/Q1Q1K3 w - - 0 1", "a1b2"), "Qa1b2");
    }

    #[test]
    fn san_marks_check_and_mate() {
        assert_eq!(san("4k3/8/8/8/8/8/8/R3K3 w - - 0 1", "a1a8"), "Ra8+");
        assert_eq!(san("6k1/5ppp/8/8/8/8/8/R5K1 w - - 0 1", "a1a8"), "Ra8#");
    }

    #[test]
    fn san_writes_pawn_captures_promotions_and_castling() {
        assert_eq!(san("4k3/8/8/3p4/4P3/8/8/4K3 w - - 0 1", "e4d5"), "exd5");
        assert_eq!(san("1n2k3/P7/8/8/8/8/8/4K3 w - - 0 1", "a7b8q"), "axb8=Q+");
        assert_eq!(san("r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1", "e1g1"), "O-O");
        assert_eq!(san("r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1", "e1c1"), "O-O-O");
    }

    #[test]
    fn unmaking_restores_the_key_whatever_the_history_holds() {
        let mut board = Board::from_fen(START_FEN).unwrap();
        board.history.clear();
        let before = board.zobrist;
        let mut legal = MoveList::new();
        board.generate_legal_moves(&mut legal);
        let m = legal.get(0);

        let undo = board.make_move(m);
        board.unmake_move(m, undo);
        let null_undo = board.make_null_move();
        board.unmake_null_move(null_undo);

        assert_eq!(board.zobrist, before);
    }

    #[test]
    fn insufficient_material_covers_lone_minors_and_same_colored_bishops() {
        let dead = [
            "8/2k5/4K3/8/8/8/8/8 w - - 0 1",
            "8/2k5/4K3/8/8/8/8/2N5 w - - 1 126",
            "8/8/2kb4/8/8/8/2K5/8 b - - 1 123",
            "8/8/2kb4/8/8/8/2K5/4B3 w - - 0 1",
        ];
        let alive = [
            "8/8/2kb4/8/8/8/2K5/3B4 w - - 0 1",
            "8/8/2kn4/8/8/8/2K5/3N4 w - - 0 1",
            "8/8/2k5/8/8/8/2K5/2NN4 w - - 0 1",
            "8/8/2k5/8/8/8/2KP4/8 w - - 0 1",
            "8/8/2k5/8/8/8/2K5/7R w - - 0 1",
        ];
        for fen in dead {
            assert!(
                Board::from_fen(fen).unwrap().has_insufficient_material(),
                "{fen}"
            );
        }
        for fen in alive {
            assert!(
                !Board::from_fen(fen).unwrap().has_insufficient_material(),
                "{fen}"
            );
        }
    }

    fn move_key(m: &Move) -> u16 {
        u16::from(*m)
    }

    /// The FENs the perft suite checks, and an en-passant position.
    const SPLIT_FENS: [&str; 6] = [
        START_FEN,
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
        "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
        "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8",
        "rnbqkbnr/ppp1pppp/8/3pP3/8/8/PPPP1PPP/RNBQKBNR w KQkq d6 0 3",
    ];

    #[test]
    fn captures_and_quiets_together_are_the_pseudo_legal_moves() {
        let fixed = SPLIT_FENS.iter().map(|fen| Board::from_fen(fen).unwrap());
        for board in fixed.chain(random_game_positions(23, 20, 80)) {
            let mut all = MoveList::new();
            board.generate_pseudo_legal_moves(&mut all);
            let mut split = MoveList::new();
            board.generate_captures(&mut split);
            let mut quiets = MoveList::new();
            board.generate_quiets(&mut quiets);
            for &m in quiets.as_slice() {
                split.push(m);
            }

            let mut all_keys: Vec<_> = all.as_slice().iter().map(move_key).collect();
            let mut split_keys: Vec<_> = split.as_slice().iter().map(move_key).collect();
            all_keys.sort();
            split_keys.sort();
            assert_eq!(all_keys, split_keys, "{}", board.to_fen());
        }
    }

    fn play(board: &mut Board, moves: &[&str]) {
        for mv in moves {
            let m = crate::uci_io::parse_uci_move(board, mv).unwrap();
            board.make_move(m);
        }
    }

    const KNIGHT_SHUFFLE: [&str; 4] = ["g1f3", "g8f6", "f3g1", "f6g8"];

    #[test]
    fn single_repeat_before_root_is_not_a_search_draw() {
        let mut board = Board::from_fen(START_FEN).unwrap();
        play(&mut board, &KNIGHT_SHUFFLE);
        assert!(!board.is_repetition(0));
    }

    #[test]
    fn single_repeat_inside_search_tree_is_a_draw() {
        let mut board = Board::from_fen(START_FEN).unwrap();
        play(&mut board, &KNIGHT_SHUFFLE);
        assert!(board.is_repetition(KNIGHT_SHUFFLE.len() + 1));
    }

    #[test]
    fn threefold_before_root_is_a_draw() {
        let mut board = Board::from_fen(START_FEN).unwrap();
        play(&mut board, &KNIGHT_SHUFFLE);
        play(&mut board, &KNIGHT_SHUFFLE);
        assert!(board.is_repetition(0));
        assert!(board.is_draw_by_repetition());
    }

    #[test]
    fn null_move_resets_and_restores_plies_from_null() {
        let mut board = Board::from_fen(START_FEN).unwrap();
        play(&mut board, &KNIGHT_SHUFFLE[..2]);
        let undo = board.make_null_move();
        assert_eq!(board.plies_from_null, 0);
        board.unmake_null_move(undo);
        assert_eq!(board.plies_from_null, 2);
    }

    #[test]
    fn repetition_does_not_look_past_a_null_move() {
        let mut board = Board::from_fen(START_FEN).unwrap();
        play(&mut board, &KNIGHT_SHUFFLE);
        board.plies_from_null = 3;
        assert!(!board.is_repetition(KNIGHT_SHUFFLE.len() + 1));
    }

    #[test]
    fn game_level_repetition_counts_position_after_last_irreversible_move() {
        let mut board = Board::from_fen(START_FEN).unwrap();
        play(&mut board, &["e2e3"]);
        play(&mut board, &["g8f6", "g1f3", "f6g8", "f3g1"]);
        play(&mut board, &["g8f6", "g1f3", "f6g8", "f3g1"]);
        assert!(board.is_draw_by_repetition());
    }
}
