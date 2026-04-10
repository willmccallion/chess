use crate::fen;
use crate::magics;
use crate::nnue;
use crate::types::*;
use crate::zobrist;

#[derive(Clone)]
pub struct Board {
    pub piece_bb: [Bitboard; 13],
    pub piece_on: [Piece; 64],
    pub w_pieces: Bitboard,
    pub b_pieces: Bitboard,
    pub all_pieces: Bitboard,
    pub turn: Color,
    pub castle: u8,
    pub en_passant_sq: i32,
    pub halfmove_clock: i32,
    pub fullmove_number: i32,
    pub history: Vec<ZKey>,
    pub zobrist: ZKey,
    pub accumulator: Accumulator,
}

impl Board {
    pub fn empty() -> Self {
        Self {
            piece_bb: [0; 13],
            piece_on: [Piece::Empty; 64],
            w_pieces: 0,
            b_pieces: 0,
            all_pieces: 0,
            turn: Color::White,
            castle: 0,
            en_passant_sq: NO_SQ,
            halfmove_clock: 0,
            fullmove_number: 1,
            history: Vec::with_capacity(128),
            zobrist: 0,
            accumulator: Accumulator::default(),
        }
    }

    #[inline(always)]
    pub fn king_square(&self, c: Color) -> u32 {
        let king_piece = Piece::from_kind(PieceKind::King, c);
        self.piece_bb[king_piece.index()].trailing_zeros()
    }

    #[inline]
    pub fn from_fen(fen_str: &str) -> Result<Self, String> {
        let mut b = fen::parse_fen(fen_str)?;
        b.accumulator = nnue::refresh_accumulator(&b);
        Ok(b)
    }

    #[inline]
    pub fn place_piece(&mut self, p: Piece, sq: usize) {
        self.piece_on[sq] = p;
    }

    #[inline]
    pub fn rebuild_derived(&mut self) {
        self.piece_bb = [0; 13];
        self.w_pieces = 0;
        self.b_pieces = 0;

        for sq in 0..64 {
            let p = self.piece_on[sq];
            if !p.is_empty() {
                self.piece_bb[p.index()] |= 1u64 << sq;
                match p.color() {
                    Some(Color::White) => self.w_pieces |= 1u64 << sq,
                    Some(Color::Black) => self.b_pieces |= 1u64 << sq,
                    _ => {}
                }
            }
        }
        self.all_pieces = self.w_pieces | self.b_pieces;
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
        if self.en_passant_sq != NO_SQ {
            h ^= zobrist::ZOB.ep_file[(self.en_passant_sq % 8) as usize];
        }
        if self.turn == Color::Black {
            h ^= zobrist::ZOB.side;
        }
        self.zobrist = h;
    }

    #[inline]
    pub fn count_repetitions(&self) -> usize {
        let current_key = self.zobrist;
        let mut count = 0;
        for &key in self
            .history
            .iter()
            .rev()
            .take(self.halfmove_clock as usize)
            .skip(1)
        {
            if key == current_key {
                count += 1;
            }
        }
        count
    }

    #[inline]
    pub fn is_draw_by_repetition(&self) -> bool {
        self.count_repetitions() >= 2
    }

    #[inline]
    pub fn is_square_attacked(&self, square: i32, by: Color) -> bool {
        let sq = square as usize;
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
        self.gen_pawns(out);
        self.gen_leapers(out);
        self.gen_sliders(out);
    }

    #[inline]
    pub fn generate_legal_moves(&mut self, out: &mut MoveList) {
        let mut pseudo = MoveList::new();
        self.generate_pseudo_legal_moves(&mut pseudo);
        out.clear();
        for &m in pseudo.as_slice() {
            let u = self.make_move(m);
            let us = self.turn.other();
            let our_king_bb = self.piece_bb[Piece::from_kind(PieceKind::King, us).index()];
            if our_king_bb == 0 {
                self.unmake_move(m, u);
                continue;
            }
            let king_sq = our_king_bb.trailing_zeros() as i32;
            if !self.is_square_attacked(king_sq, self.turn) {
                out.push(m);
            }
            self.unmake_move(m, u);
        }
    }

    fn gen_pawns(&self, out: &mut MoveList) {
        let white = self.turn == Color::White;
        let pawn = if white { Piece::WP } else { Piece::BP };
        let pawns = self.piece_bb[pawn.index()];
        let enemy = if white { self.b_pieces } else { self.w_pieces };
        let dir = if white { 8 } else { -8 };
        let start_rank = if white { 1 } else { 6 };
        let promo_rank = if white { 6 } else { 1 };
        let mut bb = pawns;

        while bb != 0 {
            let from = bb.trailing_zeros() as i32;
            bb &= bb - 1;
            let r = rank_of(from);
            let f = file_of(from);

            let to = from + dir;
            if in_board(to) && (self.all_pieces & (1u64 << to)) == 0 {
                if r == promo_rank {
                    for pk in [
                        PieceKind::Queen,
                        PieceKind::Rook,
                        PieceKind::Bishop,
                        PieceKind::Knight,
                    ] {
                        out.push(Move {
                            from: from as u8,
                            to: to as u8,
                            capture: false,
                            en_passant: false,
                            double_push: false,
                            castle: false,
                            promotion: Some(pk),
                        });
                    }
                } else {
                    out.push(Move::quiet(from as u8, to as u8));
                    if r == start_rank {
                        let to2 = from + 2 * dir;
                        if (self.all_pieces & (1u64 << to2)) == 0 {
                            out.push(Move {
                                from: from as u8,
                                to: to2 as u8,
                                capture: false,
                                en_passant: false,
                                double_push: true,
                                castle: false,
                                promotion: None,
                            });
                        }
                    }
                }
            }

            for df in [-1, 1] {
                let cap = from + dir + df;
                if (df == -1 && f == 0) || (df == 1 && f == 7) {
                    continue;
                }

                if !in_board(cap) {
                    continue;
                }

                let cap_bb = 1u64 << cap;
                if (enemy & cap_bb) != 0 {
                    if r == promo_rank {
                        for pk in [
                            PieceKind::Queen,
                            PieceKind::Rook,
                            PieceKind::Bishop,
                            PieceKind::Knight,
                        ] {
                            out.push(Move {
                                from: from as u8,
                                to: cap as u8,
                                capture: true,
                                en_passant: false,
                                double_push: false,
                                castle: false,
                                promotion: Some(pk),
                            });
                        }
                    } else {
                        out.push(Move {
                            from: from as u8,
                            to: cap as u8,
                            capture: true,
                            en_passant: false,
                            double_push: false,
                            castle: false,
                            promotion: None,
                        });
                    }
                }

                if self.en_passant_sq == cap {
                    out.push(Move {
                        from: from as u8,
                        to: cap as u8,
                        capture: true,
                        en_passant: true,
                        double_push: false,
                        castle: false,
                        promotion: None,
                    });
                }
            }
        }
    }

    #[inline(always)]
    fn first_sq(bb: u64) -> Option<i32> {
        if bb == 0 {
            None
        } else {
            Some(bb.trailing_zeros() as i32)
        }
    }

    #[inline]
    fn gen_leapers(&self, out: &mut MoveList) {
        let white = self.turn == Color::White;
        let friendly = if white { self.w_pieces } else { self.b_pieces };

        let kn = if white { Piece::WN } else { Piece::BN };
        let mut bb = self.piece_bb[kn.index()];
        while bb != 0 {
            let from = bb.trailing_zeros() as usize;
            bb &= bb - 1;

            let mut att = magics::knight_attacks_from(from) & !friendly;
            while att != 0 {
                let to = att.trailing_zeros() as usize;
                att &= att - 1;
                let capture = (self.all_pieces & (1u64 << to)) != 0;
                out.push(Move {
                    from: from as u8,
                    to: to as u8,
                    capture,
                    en_passant: false,
                    double_push: false,
                    castle: false,
                    promotion: None,
                });
            }
        }

        let king = if white { Piece::WK } else { Piece::BK };
        let king_bb = self.piece_bb[king.index()];

        let Some(from) = Self::first_sq(king_bb) else {
            return;
        };

        let mut att = magics::king_attacks_from(from as usize) & !friendly;
        while att != 0 {
            let to = att.trailing_zeros() as usize;
            att &= att - 1;
            let capture = (self.all_pieces & (1u64 << to)) != 0;
            out.push(Move {
                from: from as u8,
                to: to as u8,
                capture,
                en_passant: false,
                double_push: false,
                castle: false,
                promotion: None,
            });
        }

        if self.is_square_attacked(from, self.turn.other()) {
            return;
        }

        if white {
            if (self.castle & WK_CASTLE) != 0
                && (self.all_pieces & ((1u64 << 5) | (1u64 << 6))) == 0
                && self.piece_on[7] == Piece::WR
                && !self.is_square_attacked(5, Color::Black)
                && !self.is_square_attacked(6, Color::Black)
            {
                out.push(Move {
                    from: 4,
                    to: 6,
                    capture: false,
                    en_passant: false,
                    double_push: false,
                    castle: true,
                    promotion: None,
                });
            }

            if (self.castle & WQ_CASTLE) != 0
                && (self.all_pieces & ((1u64 << 1) | (1u64 << 2) | (1u64 << 3))) == 0
                && self.piece_on[0] == Piece::WR
                && !self.is_square_attacked(3, Color::Black)
                && !self.is_square_attacked(2, Color::Black)
            {
                out.push(Move {
                    from: 4,
                    to: 2,
                    capture: false,
                    en_passant: false,
                    double_push: false,
                    castle: true,
                    promotion: None,
                });
            }
        } else {
            if (self.castle & BK_CASTLE) != 0
                && (self.all_pieces & ((1u64 << 61) | (1u64 << 62))) == 0
                && self.piece_on[63] == Piece::BR
                && !self.is_square_attacked(61, Color::White)
                && !self.is_square_attacked(62, Color::White)
            {
                out.push(Move {
                    from: 60,
                    to: 62,
                    capture: false,
                    en_passant: false,
                    double_push: false,
                    castle: true,
                    promotion: None,
                });
            }

            if (self.castle & BQ_CASTLE) != 0
                && (self.all_pieces & ((1u64 << 57) | (1u64 << 58) | (1u64 << 59))) == 0
                && self.piece_on[56] == Piece::BR
                && !self.is_square_attacked(59, Color::White)
                && !self.is_square_attacked(58, Color::White)
            {
                out.push(Move {
                    from: 60,
                    to: 58,
                    capture: false,
                    en_passant: false,
                    double_push: false,
                    castle: true,
                    promotion: None,
                });
            }
        }
    }

    #[inline]
    fn gen_sliders(&self, out: &mut MoveList) {
        let white = self.turn == Color::White;
        let friendly = if white { self.w_pieces } else { self.b_pieces };
        let enemy = if white { self.b_pieces } else { self.w_pieces };
        let occ = self.all_pieces;
        let b_piece = if white { Piece::WB } else { Piece::BB };

        let mut bb = self.piece_bb[b_piece.index()];
        while bb != 0 {
            let from = bb.trailing_zeros() as usize;
            bb &= bb - 1;

            let mut att = magics::get_bishop_attacks(from, occ) & !friendly;
            while att != 0 {
                let to = att.trailing_zeros() as usize;
                att &= att - 1;
                let capture = (enemy & (1u64 << to)) != 0;

                out.push(Move {
                    from: from as u8,
                    to: to as u8,
                    capture,
                    en_passant: false,
                    double_push: false,
                    castle: false,
                    promotion: None,
                });
            }
        }

        let r_piece = if white { Piece::WR } else { Piece::BR };

        let mut rb = self.piece_bb[r_piece.index()];
        while rb != 0 {
            let from = rb.trailing_zeros() as usize;
            rb &= rb - 1;
            let mut att = magics::get_rook_attacks(from, occ) & !friendly;

            while att != 0 {
                let to = att.trailing_zeros() as usize;
                att &= att - 1;
                let capture = (enemy & (1u64 << to)) != 0;

                out.push(Move {
                    from: from as u8,
                    to: to as u8,
                    capture,
                    en_passant: false,
                    double_push: false,
                    castle: false,
                    promotion: None,
                });
            }
        }

        let q_piece = if white { Piece::WQ } else { Piece::BQ };

        let mut qb = self.piece_bb[q_piece.index()];
        while qb != 0 {
            let from = qb.trailing_zeros() as usize;
            qb &= qb - 1;

            let mut att = (magics::get_rook_attacks(from, occ)
                | magics::get_bishop_attacks(from, occ))
                & !friendly;

            while att != 0 {
                let to = att.trailing_zeros() as usize;
                att &= att - 1;
                let capture = (enemy & (1u64 << to)) != 0;
                out.push(Move {
                    from: from as u8,
                    to: to as u8,
                    capture,
                    en_passant: false,
                    double_push: false,
                    castle: false,
                    promotion: None,
                });
            }
        }
    }

    // ── Split generators for staged move generation ──────────────────

    pub fn generate_captures(&self, out: &mut MoveList) {
        out.clear();
        self.gen_pawn_captures(out);
        self.gen_leaper_captures(out);
        self.gen_slider_captures(out);
    }

    pub fn generate_quiets(&self, out: &mut MoveList) {
        out.clear();
        self.gen_pawn_quiets(out);
        self.gen_leaper_quiets(out);
        self.gen_slider_quiets(out);
    }

    fn gen_pawn_captures(&self, out: &mut MoveList) {
        let white = self.turn == Color::White;
        let pawn = if white { Piece::WP } else { Piece::BP };
        let pawns = self.piece_bb[pawn.index()];
        let enemy = if white { self.b_pieces } else { self.w_pieces };
        let dir: i32 = if white { 8 } else { -8 };
        let promo_rank = if white { 6 } else { 1 };
        let mut bb = pawns;

        while bb != 0 {
            let from = bb.trailing_zeros() as i32;
            bb &= bb - 1;
            let r = rank_of(from);
            let f = file_of(from);

            // Non-capture promotions (tactical, belong with captures)
            if r == promo_rank {
                let to = from + dir;
                if in_board(to) && (self.all_pieces & (1u64 << to)) == 0 {
                    for pk in [
                        PieceKind::Queen,
                        PieceKind::Rook,
                        PieceKind::Bishop,
                        PieceKind::Knight,
                    ] {
                        out.push(Move {
                            from: from as u8,
                            to: to as u8,
                            capture: false,
                            en_passant: false,
                            double_push: false,
                            castle: false,
                            promotion: Some(pk),
                        });
                    }
                }
            }

            // Diagonal captures + capture-promotions + en passant
            for df in [-1, 1] {
                let cap = from + dir + df;
                if (df == -1 && f == 0) || (df == 1 && f == 7) {
                    continue;
                }
                if !in_board(cap) {
                    continue;
                }

                let cap_bb = 1u64 << cap;
                if (enemy & cap_bb) != 0 {
                    if r == promo_rank {
                        for pk in [
                            PieceKind::Queen,
                            PieceKind::Rook,
                            PieceKind::Bishop,
                            PieceKind::Knight,
                        ] {
                            out.push(Move {
                                from: from as u8,
                                to: cap as u8,
                                capture: true,
                                en_passant: false,
                                double_push: false,
                                castle: false,
                                promotion: Some(pk),
                            });
                        }
                    } else {
                        out.push(Move {
                            from: from as u8,
                            to: cap as u8,
                            capture: true,
                            en_passant: false,
                            double_push: false,
                            castle: false,
                            promotion: None,
                        });
                    }
                }

                if self.en_passant_sq == cap {
                    out.push(Move {
                        from: from as u8,
                        to: cap as u8,
                        capture: true,
                        en_passant: true,
                        double_push: false,
                        castle: false,
                        promotion: None,
                    });
                }
            }
        }
    }

    fn gen_pawn_quiets(&self, out: &mut MoveList) {
        let white = self.turn == Color::White;
        let pawn = if white { Piece::WP } else { Piece::BP };
        let pawns = self.piece_bb[pawn.index()];
        let dir: i32 = if white { 8 } else { -8 };
        let start_rank = if white { 1 } else { 6 };
        let promo_rank = if white { 6 } else { 1 };
        let mut bb = pawns;

        while bb != 0 {
            let from = bb.trailing_zeros() as i32;
            bb &= bb - 1;
            let r = rank_of(from);

            // Skip promo-rank pawns (handled by gen_pawn_captures)
            if r == promo_rank {
                continue;
            }

            let to = from + dir;
            if in_board(to) && (self.all_pieces & (1u64 << to)) == 0 {
                out.push(Move::quiet(from as u8, to as u8));
                if r == start_rank {
                    let to2 = from + 2 * dir;
                    if (self.all_pieces & (1u64 << to2)) == 0 {
                        out.push(Move {
                            from: from as u8,
                            to: to2 as u8,
                            capture: false,
                            en_passant: false,
                            double_push: true,
                            castle: false,
                            promotion: None,
                        });
                    }
                }
            }
        }
    }

    fn gen_leaper_captures(&self, out: &mut MoveList) {
        let white = self.turn == Color::White;
        let enemy = if white { self.b_pieces } else { self.w_pieces };

        // Knights
        let kn = if white { Piece::WN } else { Piece::BN };
        let mut bb = self.piece_bb[kn.index()];
        while bb != 0 {
            let from = bb.trailing_zeros() as usize;
            bb &= bb - 1;
            let mut att = magics::knight_attacks_from(from) & enemy;
            while att != 0 {
                let to = att.trailing_zeros() as usize;
                att &= att - 1;
                out.push(Move {
                    from: from as u8,
                    to: to as u8,
                    capture: true,
                    en_passant: false,
                    double_push: false,
                    castle: false,
                    promotion: None,
                });
            }
        }

        // King captures
        let king = if white { Piece::WK } else { Piece::BK };
        let king_bb = self.piece_bb[king.index()];
        let Some(from) = Self::first_sq(king_bb) else {
            return;
        };
        let mut att = magics::king_attacks_from(from as usize) & enemy;
        while att != 0 {
            let to = att.trailing_zeros() as usize;
            att &= att - 1;
            out.push(Move {
                from: from as u8,
                to: to as u8,
                capture: true,
                en_passant: false,
                double_push: false,
                castle: false,
                promotion: None,
            });
        }
    }

    fn gen_leaper_quiets(&self, out: &mut MoveList) {
        let white = self.turn == Color::White;
        let empty = !self.all_pieces;

        // Knights
        let kn = if white { Piece::WN } else { Piece::BN };
        let mut bb = self.piece_bb[kn.index()];
        while bb != 0 {
            let from = bb.trailing_zeros() as usize;
            bb &= bb - 1;
            let mut att = magics::knight_attacks_from(from) & empty;
            while att != 0 {
                let to = att.trailing_zeros() as usize;
                att &= att - 1;
                out.push(Move::quiet(from as u8, to as u8));
            }
        }

        // King quiets
        let king = if white { Piece::WK } else { Piece::BK };
        let king_bb = self.piece_bb[king.index()];
        let Some(from) = Self::first_sq(king_bb) else {
            return;
        };
        let mut att = magics::king_attacks_from(from as usize) & empty;
        while att != 0 {
            let to = att.trailing_zeros() as usize;
            att &= att - 1;
            out.push(Move::quiet(from as u8, to as u8));
        }

        // Castling
        if self.is_square_attacked(from, self.turn.other()) {
            return;
        }

        if white {
            if (self.castle & WK_CASTLE) != 0
                && (self.all_pieces & ((1u64 << 5) | (1u64 << 6))) == 0
                && self.piece_on[7] == Piece::WR
                && !self.is_square_attacked(5, Color::Black)
                && !self.is_square_attacked(6, Color::Black)
            {
                out.push(Move {
                    from: 4,
                    to: 6,
                    capture: false,
                    en_passant: false,
                    double_push: false,
                    castle: true,
                    promotion: None,
                });
            }
            if (self.castle & WQ_CASTLE) != 0
                && (self.all_pieces & ((1u64 << 1) | (1u64 << 2) | (1u64 << 3))) == 0
                && self.piece_on[0] == Piece::WR
                && !self.is_square_attacked(3, Color::Black)
                && !self.is_square_attacked(2, Color::Black)
            {
                out.push(Move {
                    from: 4,
                    to: 2,
                    capture: false,
                    en_passant: false,
                    double_push: false,
                    castle: true,
                    promotion: None,
                });
            }
        } else {
            if (self.castle & BK_CASTLE) != 0
                && (self.all_pieces & ((1u64 << 61) | (1u64 << 62))) == 0
                && self.piece_on[63] == Piece::BR
                && !self.is_square_attacked(61, Color::White)
                && !self.is_square_attacked(62, Color::White)
            {
                out.push(Move {
                    from: 60,
                    to: 62,
                    capture: false,
                    en_passant: false,
                    double_push: false,
                    castle: true,
                    promotion: None,
                });
            }
            if (self.castle & BQ_CASTLE) != 0
                && (self.all_pieces & ((1u64 << 57) | (1u64 << 58) | (1u64 << 59))) == 0
                && self.piece_on[56] == Piece::BR
                && !self.is_square_attacked(59, Color::White)
                && !self.is_square_attacked(58, Color::White)
            {
                out.push(Move {
                    from: 60,
                    to: 58,
                    capture: false,
                    en_passant: false,
                    double_push: false,
                    castle: true,
                    promotion: None,
                });
            }
        }
    }

    fn gen_slider_captures(&self, out: &mut MoveList) {
        let white = self.turn == Color::White;
        let enemy = if white { self.b_pieces } else { self.w_pieces };
        let occ = self.all_pieces;

        // Bishops
        let b_piece = if white { Piece::WB } else { Piece::BB };
        let mut bb = self.piece_bb[b_piece.index()];
        while bb != 0 {
            let from = bb.trailing_zeros() as usize;
            bb &= bb - 1;
            let mut att = magics::get_bishop_attacks(from, occ) & enemy;
            while att != 0 {
                let to = att.trailing_zeros() as usize;
                att &= att - 1;
                out.push(Move {
                    from: from as u8,
                    to: to as u8,
                    capture: true,
                    en_passant: false,
                    double_push: false,
                    castle: false,
                    promotion: None,
                });
            }
        }

        // Rooks
        let r_piece = if white { Piece::WR } else { Piece::BR };
        let mut rb = self.piece_bb[r_piece.index()];
        while rb != 0 {
            let from = rb.trailing_zeros() as usize;
            rb &= rb - 1;
            let mut att = magics::get_rook_attacks(from, occ) & enemy;
            while att != 0 {
                let to = att.trailing_zeros() as usize;
                att &= att - 1;
                out.push(Move {
                    from: from as u8,
                    to: to as u8,
                    capture: true,
                    en_passant: false,
                    double_push: false,
                    castle: false,
                    promotion: None,
                });
            }
        }

        // Queens
        let q_piece = if white { Piece::WQ } else { Piece::BQ };
        let mut qb = self.piece_bb[q_piece.index()];
        while qb != 0 {
            let from = qb.trailing_zeros() as usize;
            qb &= qb - 1;
            let mut att = (magics::get_rook_attacks(from, occ)
                | magics::get_bishop_attacks(from, occ))
                & enemy;
            while att != 0 {
                let to = att.trailing_zeros() as usize;
                att &= att - 1;
                out.push(Move {
                    from: from as u8,
                    to: to as u8,
                    capture: true,
                    en_passant: false,
                    double_push: false,
                    castle: false,
                    promotion: None,
                });
            }
        }
    }

    fn gen_slider_quiets(&self, out: &mut MoveList) {
        let white = self.turn == Color::White;
        let occ = self.all_pieces;
        let empty = !occ;

        // Bishops
        let b_piece = if white { Piece::WB } else { Piece::BB };
        let mut bb = self.piece_bb[b_piece.index()];
        while bb != 0 {
            let from = bb.trailing_zeros() as usize;
            bb &= bb - 1;
            let mut att = magics::get_bishop_attacks(from, occ) & empty;
            while att != 0 {
                let to = att.trailing_zeros() as usize;
                att &= att - 1;
                out.push(Move::quiet(from as u8, to as u8));
            }
        }

        // Rooks
        let r_piece = if white { Piece::WR } else { Piece::BR };
        let mut rb = self.piece_bb[r_piece.index()];
        while rb != 0 {
            let from = rb.trailing_zeros() as usize;
            rb &= rb - 1;
            let mut att = magics::get_rook_attacks(from, occ) & empty;
            while att != 0 {
                let to = att.trailing_zeros() as usize;
                att &= att - 1;
                out.push(Move::quiet(from as u8, to as u8));
            }
        }

        // Queens
        let q_piece = if white { Piece::WQ } else { Piece::BQ };
        let mut qb = self.piece_bb[q_piece.index()];
        while qb != 0 {
            let from = qb.trailing_zeros() as usize;
            qb &= qb - 1;
            let mut att = (magics::get_rook_attacks(from, occ)
                | magics::get_bishop_attacks(from, occ))
                & empty;
            while att != 0 {
                let to = att.trailing_zeros() as usize;
                att &= att - 1;
                out.push(Move::quiet(from as u8, to as u8));
            }
        }
    }

    /// Check if a move is pseudo-legal in the current position.
    /// Must match every assumption that `make_move` relies on.
    pub fn is_pseudo_legal(&self, m: Move) -> bool {
        let from = m.from as usize;
        let to = m.to as usize;
        if from >= 64 || to >= 64 || from == to {
            return false;
        }

        let piece = self.piece_on[from];
        if piece == Piece::Empty || piece.color() != Some(self.turn) {
            return false;
        }
        let kind = match piece.kind() {
            Some(k) => k,
            None => return false,
        };

        // Flag consistency: special flags require specific piece types
        if m.castle && kind != PieceKind::King {
            return false;
        }
        if (m.en_passant || m.double_push || m.promotion.is_some()) && kind != PieceKind::Pawn {
            return false;
        }

        // ── Castling ────────────────────────────────────────────────
        // Must replicate every check from gen_leapers/gen_leaper_quiets
        // because make_move blindly moves the rook from a computed square.
        if m.castle {
            let white = self.turn == Color::White;
            let expected_from: usize = if white { 4 } else { 60 };
            if from != expected_from {
                return false;
            }
            if self.is_square_attacked(from as i32, self.turn.other()) {
                return false;
            }
            return if white {
                match to {
                    6 => {
                        (self.castle & WK_CASTLE) != 0
                            && self.piece_on[7] == Piece::WR
                            && (self.all_pieces & ((1u64 << 5) | (1u64 << 6))) == 0
                            && !self.is_square_attacked(5, Color::Black)
                            && !self.is_square_attacked(6, Color::Black)
                    }
                    2 => {
                        (self.castle & WQ_CASTLE) != 0
                            && self.piece_on[0] == Piece::WR
                            && (self.all_pieces & ((1u64 << 1) | (1u64 << 2) | (1u64 << 3))) == 0
                            && !self.is_square_attacked(3, Color::Black)
                            && !self.is_square_attacked(2, Color::Black)
                    }
                    _ => false,
                }
            } else {
                match to {
                    62 => {
                        (self.castle & BK_CASTLE) != 0
                            && self.piece_on[63] == Piece::BR
                            && (self.all_pieces & ((1u64 << 61) | (1u64 << 62))) == 0
                            && !self.is_square_attacked(61, Color::White)
                            && !self.is_square_attacked(62, Color::White)
                    }
                    58 => {
                        (self.castle & BQ_CASTLE) != 0
                            && self.piece_on[56] == Piece::BR
                            && (self.all_pieces
                                & ((1u64 << 57) | (1u64 << 58) | (1u64 << 59)))
                                == 0
                            && !self.is_square_attacked(59, Color::White)
                            && !self.is_square_attacked(58, Color::White)
                    }
                    _ => false,
                }
            };
        }

        // ── Target square ───────────────────────────────────────────
        let to_piece = self.piece_on[to];
        if m.capture {
            if m.en_passant {
                // En-passant: target square must be empty (pawn is beside us),
                // en_passant_sq must match, and the captured pawn must exist.
                if self.en_passant_sq != to as i32 {
                    return false;
                }
                if to_piece != Piece::Empty {
                    return false;
                }
                let cap_sq = if self.turn == Color::White {
                    to as i32 - 8
                } else {
                    to as i32 + 8
                };
                if cap_sq < 0 || cap_sq >= 64 {
                    return false;
                }
                let enemy_pawn = if self.turn == Color::White {
                    Piece::BP
                } else {
                    Piece::WP
                };
                if self.piece_on[cap_sq as usize] != enemy_pawn {
                    return false;
                }
            } else {
                // Regular capture: target must be an enemy piece.
                if to_piece == Piece::Empty || to_piece.color() == Some(self.turn) {
                    return false;
                }
            }
        } else {
            // Non-capture: target must be empty.
            if to_piece != Piece::Empty {
                return false;
            }
        }

        // ── Piece-specific geometry ─────────────────────────────────
        let occ = self.all_pieces;
        match kind {
            PieceKind::Knight => (magics::knight_attacks_from(from) & (1u64 << to)) != 0,
            PieceKind::Bishop => (magics::get_bishop_attacks(from, occ) & (1u64 << to)) != 0,
            PieceKind::Rook => (magics::get_rook_attacks(from, occ) & (1u64 << to)) != 0,
            PieceKind::Queen => {
                ((magics::get_bishop_attacks(from, occ)
                    | magics::get_rook_attacks(from, occ))
                    & (1u64 << to))
                    != 0
            }
            PieceKind::King => (magics::king_attacks_from(from) & (1u64 << to)) != 0,
            PieceKind::Pawn => {
                let white = self.turn == Color::White;
                let dir: i32 = if white { 8 } else { -8 };
                let from_i = from as i32;
                let to_i = to as i32;

                if m.capture {
                    let diff = to_i - from_i;
                    if diff != dir - 1 && diff != dir + 1 {
                        return false;
                    }
                } else if m.double_push {
                    let start_rank = if white { 1 } else { 6 };
                    let mid = from_i + dir;
                    if rank_of(from_i) != start_rank
                        || to_i != from_i + 2 * dir
                        || (self.all_pieces & (1u64 << mid)) != 0
                    {
                        return false;
                    }
                } else {
                    if to_i != from_i + dir {
                        return false;
                    }
                }

                // Promotion rank consistency
                if m.promotion.is_some() {
                    let promo_rank = if white { 6 } else { 1 };
                    if rank_of(from_i) != promo_rank {
                        return false;
                    }
                }
                true
            }
        }
    }

    #[inline]
    pub fn make_move(&mut self, m: Move) -> Undo {
        let mut undo = Undo {
            captured_piece: Piece::Empty,
            old_castle: self.castle,
            old_en_passant_sq: self.en_passant_sq,
            old_halfmove_clock: self.halfmove_clock,
        };

        let mut updates = [UpdateBody {
            piece: Piece::Empty,
            sq: 0,
            add: false,
        }; 5];
        let mut update_count = 0;

        let from = m.from as usize;
        let to = m.to as usize;
        let moving = self.piece_on[from];
        let is_king_move = moving.kind() == Some(PieceKind::King);

        updates[update_count] = UpdateBody {
            piece: moving,
            sq: from,
            add: false,
        };
        update_count += 1;

        if m.capture {
            let cap_sq = if m.en_passant {
                if self.turn == Color::White {
                    to - 8
                } else {
                    to + 8
                }
            } else {
                to
            };
            let captured = self.piece_on[cap_sq];
            undo.captured_piece = captured;

            if !captured.is_empty() {
                updates[update_count] = UpdateBody {
                    piece: captured,
                    sq: cap_sq,
                    add: false,
                };
                update_count += 1;
            }
        }

        let piece_to_add = if let Some(pk) = m.promotion {
            Piece::from_kind(pk, self.turn)
        } else {
            moving
        };
        updates[update_count] = UpdateBody {
            piece: piece_to_add,
            sq: to,
            add: true,
        };
        update_count += 1;

        if m.castle {
            let (rook_from, rook_to) = if to > from {
                (to + 1, to - 1)
            } else {
                (to - 2, to + 1)
            };
            let rook_piece = self.piece_on[rook_from];

            updates[update_count] = UpdateBody {
                piece: rook_piece,
                sq: rook_from,
                add: false,
            };
            update_count += 1;
            updates[update_count] = UpdateBody {
                piece: rook_piece,
                sq: rook_to,
                add: true,
            };
            update_count += 1;
        }

        if self.en_passant_sq != NO_SQ {
            self.zobrist ^= zobrist::ZOB.ep_file[(self.en_passant_sq % 8) as usize];
        }
        self.en_passant_sq = NO_SQ;

        self.zobrist ^= zobrist::ZOB.piece_key(moving, from);
        self.piece_on[from] = Piece::Empty;
        self.piece_bb[moving.index()] ^= 1u64 << from;

        match moving.color() {
            Some(Color::White) => self.w_pieces ^= 1u64 << from,
            Some(Color::Black) => self.b_pieces ^= 1u64 << from,
            _ => {}
        }

        if m.capture {
            let cap_sq = if m.en_passant {
                if self.turn == Color::White {
                    to - 8
                } else {
                    to + 8
                }
            } else {
                to
            };

            let captured = undo.captured_piece;
            if !captured.is_empty() {
                self.zobrist ^= zobrist::ZOB.piece_key(captured, cap_sq);
                self.piece_on[cap_sq] = Piece::Empty;
                self.piece_bb[captured.index()] ^= 1u64 << cap_sq;
                match captured.color() {
                    Some(Color::White) => self.w_pieces ^= 1u64 << cap_sq,
                    Some(Color::Black) => self.b_pieces ^= 1u64 << cap_sq,
                    _ => {}
                }
            }
        }

        if let Some(pk) = m.promotion {
            let promoted_piece = Piece::from_kind(pk, self.turn);
            self.piece_on[to] = promoted_piece;
            self.piece_bb[promoted_piece.index()] |= 1u64 << to;
            self.zobrist ^= zobrist::ZOB.piece_key(promoted_piece, to);
        } else {
            self.piece_on[to] = moving;
            self.piece_bb[moving.index()] |= 1u64 << to;
            self.zobrist ^= zobrist::ZOB.piece_key(moving, to);
        }

        match moving.color() {
            Some(Color::White) => self.w_pieces |= 1u64 << to,
            Some(Color::Black) => self.b_pieces |= 1u64 << to,
            _ => {}
        }

        if m.castle {
            let (rook_from, rook_to) = if to > from {
                (to + 1, to - 1)
            } else {
                (to - 2, to + 1)
            };
            let rook_piece = self.piece_on[rook_from];
            self.zobrist ^= zobrist::ZOB.piece_key(rook_piece, rook_from);
            self.zobrist ^= zobrist::ZOB.piece_key(rook_piece, rook_to);
            self.piece_on[rook_from] = Piece::Empty;
            self.piece_on[rook_to] = rook_piece;

            let rook_bb = (1u64 << rook_from) | (1u64 << rook_to);
            self.piece_bb[rook_piece.index()] ^= rook_bb;

            match rook_piece.color().unwrap() {
                Color::White => self.w_pieces ^= rook_bb,
                Color::Black => self.b_pieces ^= rook_bb,
            }
        }

        if m.double_push {
            let ep = if self.turn == Color::White {
                from + 8
            } else {
                from - 8
            };
            self.en_passant_sq = ep as i32;
            self.zobrist ^= zobrist::ZOB.ep_file[ep % 8];
        }

        if matches!(moving.kind(), Some(PieceKind::Pawn)) || m.capture {
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
        if m.capture {
            match to {
                0 => self.castle &= !WQ_CASTLE,
                7 => self.castle &= !WK_CASTLE,
                56 => self.castle &= !BQ_CASTLE,
                63 => self.castle &= !BK_CASTLE,
                _ => {}
            }
        }
        self.zobrist ^= zobrist::ZOB.castle[(self.castle & 0xF) as usize];

        self.all_pieces = self.w_pieces | self.b_pieces;
        self.zobrist ^= zobrist::ZOB.side;
        if self.turn == Color::Black {
            self.fullmove_number += 1;
        }

        self.turn = self.turn.other();
        self.history.push(self.zobrist);

        if is_king_move {
            self.accumulator = nnue::refresh_accumulator(self);
        } else {
            let wk = self.king_square(Color::White) as usize;
            let bk = self.king_square(Color::Black) as usize;
            nnue::update_accumulator(&mut self.accumulator, wk, bk, &updates[..update_count]);
        }

        undo
    }

    #[inline]
    pub fn unmake_move(&mut self, m: Move, u: Undo) {
        let mut updates = [UpdateBody {
            piece: Piece::Empty,
            sq: 0,
            add: false,
        }; 5];
        let mut update_count = 0;

        let from = m.from as usize;
        let to = m.to as usize;

        let piece_on_to = self.piece_on[to];
        let moving_piece = if m.promotion.is_some() {
            Piece::from_kind(PieceKind::Pawn, self.turn.other())
        } else {
            piece_on_to
        };
        let is_king_move = moving_piece.kind() == Some(PieceKind::King);

        updates[update_count] = UpdateBody {
            piece: piece_on_to,
            sq: to,
            add: false,
        };
        update_count += 1;

        updates[update_count] = UpdateBody {
            piece: moving_piece,
            sq: from,
            add: true,
        };
        update_count += 1;

        if m.capture {
            let cap_sq = if m.en_passant {
                if self.turn == Color::White {
                    to + 8
                } else {
                    to - 8
                }
            } else {
                to
            };
            let captured = u.captured_piece;
            if !captured.is_empty() {
                updates[update_count] = UpdateBody {
                    piece: captured,
                    sq: cap_sq,
                    add: true,
                };
                update_count += 1;
            }
        }

        if m.castle {
            let (rook_from, rook_to) = if to > from {
                (to + 1, to - 1)
            } else {
                (to - 2, to + 1)
            };
            let rook_piece = self.piece_on[rook_to];

            updates[update_count] = UpdateBody {
                piece: rook_piece,
                sq: rook_to,
                add: false,
            };
            update_count += 1;
            updates[update_count] = UpdateBody {
                piece: rook_piece,
                sq: rook_from,
                add: true,
            };
            update_count += 1;
        }

        if !is_king_move {
            let wk = self.king_square(Color::White) as usize;
            let bk = self.king_square(Color::Black) as usize;
            nnue::update_accumulator(&mut self.accumulator, wk, bk, &updates[..update_count]);
        }

        self.history.pop();
        self.zobrist = *self.history.last().unwrap_or(&0);

        self.turn = self.turn.other();
        if self.turn == Color::Black {
            self.fullmove_number -= 1;
        }

        self.castle = u.old_castle;
        self.en_passant_sq = u.old_en_passant_sq;
        self.halfmove_clock = u.old_halfmove_clock;

        self.piece_on[from] = moving_piece;
        self.piece_bb[moving_piece.index()] |= 1u64 << from;
        if let Some(c) = moving_piece.color() {
            if c == Color::White {
                self.w_pieces |= 1u64 << from;
            } else {
                self.b_pieces |= 1u64 << from;
            }
        }

        self.piece_bb[piece_on_to.index()] &= !(1u64 << to);
        if let Some(c) = piece_on_to.color() {
            if c == Color::White {
                self.w_pieces &= !(1u64 << to);
            } else {
                self.b_pieces &= !(1u64 << to);
            }
        }

        if m.capture {
            let captured = u.captured_piece;
            let cap_sq;
            if m.en_passant {
                self.piece_on[to] = Piece::Empty;
                cap_sq = if self.turn == Color::White {
                    to - 8
                } else {
                    to + 8
                };
            } else {
                cap_sq = to;
            }

            self.piece_on[cap_sq] = captured;
            if !captured.is_empty() {
                self.piece_bb[captured.index()] |= 1u64 << cap_sq;
                if let Some(c) = captured.color() {
                    if c == Color::White {
                        self.w_pieces |= 1u64 << cap_sq;
                    } else {
                        self.b_pieces |= 1u64 << cap_sq;
                    }
                }
            }
        } else {
            self.piece_on[to] = Piece::Empty;
        }

        if m.castle {
            let (rook_from, rook_to) = if to > from {
                (to + 1, to - 1)
            } else {
                (to - 2, to + 1)
            };
            let rook = self.piece_on[rook_to];
            self.piece_on[rook_from] = rook;
            self.piece_on[rook_to] = Piece::Empty;

            let rook_bb = (1u64 << rook_from) | (1u64 << rook_to);
            self.piece_bb[rook.index()] ^= rook_bb;
            match rook.color().unwrap() {
                Color::White => self.w_pieces ^= rook_bb,
                Color::Black => self.b_pieces ^= rook_bb,
            }
        }

        self.all_pieces = self.w_pieces | self.b_pieces;

        if is_king_move {
            self.accumulator = nnue::refresh_accumulator(self);
        }
    }

    #[inline]
    pub fn make_null_move(&mut self) -> Undo {
        let undo = Undo {
            captured_piece: Piece::Empty,
            old_castle: self.castle,
            old_en_passant_sq: self.en_passant_sq,
            old_halfmove_clock: self.halfmove_clock,
        };

        if self.en_passant_sq != NO_SQ {
            self.zobrist ^= zobrist::ZOB.ep_file[(self.en_passant_sq % 8) as usize];
            self.en_passant_sq = NO_SQ;
        }

        self.turn = self.turn.other();
        self.zobrist ^= zobrist::ZOB.side;
        self.halfmove_clock += 1;
        self.history.push(self.zobrist);

        undo
    }

    #[inline]
    pub fn unmake_null_move(&mut self, u: Undo) {
        self.history.pop();
        self.zobrist = *self.history.last().unwrap_or(&0);
        self.turn = self.turn.other();
        self.en_passant_sq = u.old_en_passant_sq;
        self.halfmove_clock = u.old_halfmove_clock;
    }

    #[inline]
    pub fn to_fen(&self) -> String {
        fen::to_fen(self)
    }

    pub fn to_san(&self, m: Move, legal_moves: &[Move]) -> String {
        if m.castle {
            return if m.to > m.from { "O-O" } else { "O-O-O" }.to_string();
        }

        let from = m.from as usize;
        let to = m.to as usize;
        let moving_piece = self.piece_on[from];
        let mut san = String::new();

        if let Some(pk) = moving_piece.kind() {
            match pk {
                PieceKind::Pawn => {
                    if m.capture {
                        san.push(file_char(from));
                    }
                }
                _ => {
                    san.push(pk.to_char_upper());
                    let mut ambiguous_moves = Vec::new();
                    for other_move in legal_moves {
                        let other_from = other_move.from as usize;
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
                            if file_char(amb_move.from as usize) == file_char(from) {
                                file_is_unique = false;
                            }
                            if rank_char(amb_move.from as usize) == rank_char(from) {
                                rank_is_unique = false;
                            }
                        }

                        if file_is_unique {
                            san.push(file_char(from));
                        } else if rank_is_unique {
                            san.push(rank_char(from));
                        } else {
                            san.push_str(&sq_to_str(from));
                        }
                    }
                }
            }
        }

        if m.capture {
            san.push('x');
        }

        san.push_str(&sq_to_str(to));

        if let Some(promo) = m.promotion {
            san.push('=');
            san.push(promo.to_char_upper());
        }

        let mut temp_board = self.clone();
        let undo = temp_board.make_move(m);

        let opp_king_sq = temp_board.piece_bb
            [Piece::from_kind(PieceKind::King, temp_board.turn).index()]
        .trailing_zeros() as i32;

        if temp_board.is_square_attacked(opp_king_sq, self.turn) {
            let mut has_legal_move = false;
            let mut next_moves = MoveList::new();
            temp_board.generate_legal_moves(&mut next_moves);

            if !next_moves.is_empty() {
                has_legal_move = true;
            }

            if has_legal_move {
                san.push('+');
            } else {
                san.push('#');
            }
        }

        temp_board.unmake_move(m, undo);

        san
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn move_key(m: &Move) -> (u8, u8, bool, bool, bool, bool, Option<PieceKind>) {
        (m.from, m.to, m.capture, m.en_passant, m.double_push, m.castle, m.promotion)
    }

    fn assert_split_equals_all(fen: &str) {
        let b = Board::from_fen(fen).unwrap();

        let mut all = MoveList::new();
        b.generate_pseudo_legal_moves(&mut all);

        let mut caps = MoveList::new();
        b.generate_captures(&mut caps);

        let mut quiets = MoveList::new();
        b.generate_quiets(&mut quiets);

        let mut all_sorted: Vec<_> = all.as_slice().iter().map(move_key).collect();
        all_sorted.sort();

        let mut split_sorted: Vec<_> = caps
            .as_slice()
            .iter()
            .chain(quiets.as_slice().iter())
            .map(move_key)
            .collect();
        split_sorted.sort();

        assert_eq!(
            all_sorted, split_sorted,
            "Split mismatch for FEN: {}.\n all={} captures={} quiets={}",
            fen,
            all.len(),
            caps.len(),
            quiets.len()
        );
    }

    #[test]
    fn test_split_generators_startpos() {
        assert_split_equals_all("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
    }

    #[test]
    fn test_split_generators_kiwipete() {
        assert_split_equals_all(
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        );
    }

    #[test]
    fn test_split_generators_position3() {
        assert_split_equals_all("8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1");
    }

    #[test]
    fn test_split_generators_position4() {
        assert_split_equals_all(
            "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
        );
    }

    #[test]
    fn test_split_generators_position5() {
        assert_split_equals_all("rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8");
    }

    #[test]
    fn test_split_generators_en_passant() {
        assert_split_equals_all("rnbqkbnr/ppp1pppp/8/3pP3/8/8/PPPP1PPP/RNBQKBNR w KQkq d6 0 3");
    }
}
