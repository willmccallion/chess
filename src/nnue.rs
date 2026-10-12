use crate::board::Board;
use crate::types::{
    Accumulator, Bitboard, Color, NNUE_FEATURE_SIZE, Piece, PieceKind, StackVec, UpdateBody,
};
use byteorder::{LittleEndian, ReadBytesExt};
use once_cell::sync::OnceCell;
use std::error::Error;
use std::fmt;
use std::io::{BufReader, Cursor, Read, Seek};

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

const SQUARE_NB: usize = 64;
const FT_INPUT_DIM: usize = 41024;
const HL1_INPUT_DIM: usize = 512;
const HL1_OUTPUT_DIM: usize = 32;
const HL2_OUTPUT_DIM: usize = 32;
const WEIGHT_SCALE_BITS: u32 = 6;

pub struct Model {
    ft_weights: Vec<i16>,
    ft_biases: Vec<i16>,
    hl1_weights: Vec<i8>,
    hl1_biases: Vec<i32>,
    hl2_weights: Vec<i8>,
    hl2_biases: Vec<i32>,
    out_weights: Vec<i8>,
    out_bias: i32,
}

static MODEL: OnceCell<Model> = OnceCell::new();

#[derive(Debug)]
pub enum NnueError {
    IoError(std::io::Error),
    ValueError(String),
    AlreadyInitialized,
}

impl fmt::Display for NnueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NnueError::IoError(e) => write!(f, "I/O Error: {}", e),
            NnueError::ValueError(msg) => write!(f, "Value Error: {}", msg),
            NnueError::AlreadyInitialized => write!(f, "Model has already been initialized!"),
        }
    }
}

impl Error for NnueError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            NnueError::IoError(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for NnueError {
    fn from(e: std::io::Error) -> Self {
        NnueError::IoError(e)
    }
}

pub fn init() -> Result<(), NnueError> {
    const NNUE_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/nn-9931db908a9b.nnue"));
    let mut reader = BufReader::new(Cursor::new(NNUE_BYTES));

    let _version = reader.read_u32::<LittleEndian>()?;
    let _hash_value = reader.read_u32::<LittleEndian>()?;
    let desc_size = reader.read_u32::<LittleEndian>()? as usize;
    let mut desc_bytes = vec![0u8; desc_size];
    reader.read_exact(&mut desc_bytes)?;

    let ft_header = reader.read_u32::<LittleEndian>()?;
    let expected_ft_hash = (0x5D69D5B9_u32 ^ 1) ^ (2 * NNUE_FEATURE_SIZE as u32);
    if ft_header != expected_ft_hash {
        return Err(NnueError::ValueError(
            "Feature transformer header does not match expected hash!".to_string(),
        ));
    }

    let mut ft_biases = vec![0i16; NNUE_FEATURE_SIZE];
    reader.read_i16_into::<LittleEndian>(&mut ft_biases)?;
    let ft_weights_count = NNUE_FEATURE_SIZE * FT_INPUT_DIM;
    let mut ft_weights = vec![0i16; ft_weights_count];
    reader.read_i16_into::<LittleEndian>(&mut ft_weights)?;

    let _l1_header = reader.read_u32::<LittleEndian>()?;
    let mut hl1_biases = vec![0i32; HL1_OUTPUT_DIM];
    reader.read_i32_into::<LittleEndian>(&mut hl1_biases)?;
    let hl1_weights_count = HL1_INPUT_DIM * HL1_OUTPUT_DIM;
    let mut hl1_weights = vec![0i8; hl1_weights_count];
    reader.read_i8_into(&mut hl1_weights)?;

    let mut hl2_biases = vec![0i32; HL2_OUTPUT_DIM];
    reader.read_i32_into::<LittleEndian>(&mut hl2_biases)?;
    let hl2_weights_count = HL2_OUTPUT_DIM * HL2_OUTPUT_DIM;
    let mut hl2_weights = vec![0i8; hl2_weights_count];
    reader.read_i8_into(&mut hl2_weights)?;

    let out_bias = reader.read_i32::<LittleEndian>()?;
    let mut out_weights = vec![0i8; HL2_OUTPUT_DIM];
    reader.read_i8_into(&mut out_weights)?;

    let current_pos = reader.stream_position()?;
    let end_pos = reader.get_ref().get_ref().len() as u64;
    if end_pos - current_pos != 0 {
        return Err(NnueError::ValueError(
            "Did not read all parameters from NNUE file!".to_string(),
        ));
    }

    let model = Model {
        ft_weights,
        ft_biases,
        hl1_weights,
        hl1_biases,
        hl2_weights,
        hl2_biases,
        out_weights,
        out_bias,
    };

    MODEL
        .set(model)
        .map_err(|_| NnueError::AlreadyInitialized)?;
    Ok(())
}

#[inline(always)]
fn perspective_half(acc: &mut Accumulator, perspective: Color) -> &mut [i16; NNUE_FEATURE_SIZE] {
    match perspective {
        Color::White => &mut acc.white,
        Color::Black => &mut acc.black,
    }
}

const MAX_REFRESH_FEATURES: usize = 32;
const MAX_UPDATE_FEATURES: usize = 4;
/// A move changes at most five piece-square entries (castling with a capture is impossible).
pub const MAX_MOVE_UPDATES: usize = 5;

/// The board state a from-scratch refresh needs.
pub struct RefreshContext<'a> {
    pub piece_on: &'a [Piece; 64],
    pub non_king_pieces: Bitboard,
    pub king_squares: [usize; 2],
}

/// Computes the accumulator from scratch for both perspectives.
pub fn refresh_accumulator(board: &Board) -> Accumulator {
    let ctx = board.refresh_context();
    let mut acc = Accumulator::default();
    for perspective in [Color::White, Color::Black] {
        refresh_perspective(perspective_half(&mut acc, perspective), perspective, &ctx);
    }
    acc
}

fn refresh_perspective(
    half: &mut [i16; NNUE_FEATURE_SIZE],
    perspective: Color,
    ctx: &RefreshContext,
) {
    let Some(model) = MODEL.get() else {
        return;
    };
    let is_white_pov = perspective == Color::White;
    let king_sq = ctx.king_squares[perspective as usize];
    let mut features: StackVec<usize, MAX_REFRESH_FEATURES> = StackVec::default();
    let mut pieces = ctx.non_king_pieces;
    while pieces != 0 {
        let sq = pieces.trailing_zeros() as usize;
        pieces &= pieces - 1;
        let piece = ctx.piece_on[sq];
        if let Some(color) = piece.color() {
            features.push(make_halfkp_index(is_white_pov, king_sq, sq, piece, color));
        }
    }
    half.copy_from_slice(&model.ft_biases);
    let source = *half;
    apply_features(&source, half, &model.ft_weights, features.as_slice(), &[]);
}

/// HalfKP feature indices added and removed by a move, from one perspective.
fn changed_features(
    perspective: Color,
    king_sq: usize,
    updates: &[UpdateBody],
) -> (
    StackVec<usize, MAX_UPDATE_FEATURES>,
    StackVec<usize, MAX_UPDATE_FEATURES>,
) {
    let is_white_pov = perspective == Color::White;
    let mut added = StackVec::default();
    let mut removed = StackVec::default();
    for u in updates {
        if u.piece.kind() == Some(PieceKind::King) {
            continue;
        }
        let Some(color) = u.piece.color() else {
            continue;
        };
        let idx = make_halfkp_index(is_white_pov, king_sq, u.sq, u.piece, color);
        if u.add {
            added.push(idx);
        } else {
            removed.push(idx);
        }
    }
    (added, removed)
}

/// Writes `source` plus the added rows minus the removed rows into `target` in one pass.
#[inline(always)]
fn apply_features(
    source: &[i16; NNUE_FEATURE_SIZE],
    target: &mut [i16; NNUE_FEATURE_SIZE],
    weights: &[i16],
    added: &[usize],
    removed: &[usize],
) {
    let row = |idx: usize| &weights[idx * NNUE_FEATURE_SIZE..(idx + 1) * NNUE_FEATURE_SIZE];
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    {
        const LANES: usize = 16;
        for chunk in 0..NNUE_FEATURE_SIZE / LANES {
            let offset = chunk * LANES;
            // SAFETY: AVX2 is enabled at compile time; every access touches LANES i16
            // values at `offset`, inside the 256-lane accumulators and each 256-lane row.
            unsafe {
                let mut v = _mm256_loadu_si256(source.as_ptr().add(offset).cast());
                for &idx in added {
                    let w = _mm256_loadu_si256(row(idx).as_ptr().add(offset).cast());
                    v = _mm256_add_epi16(v, w);
                }
                for &idx in removed {
                    let w = _mm256_loadu_si256(row(idx).as_ptr().add(offset).cast());
                    v = _mm256_sub_epi16(v, w);
                }
                _mm256_storeu_si256(target.as_mut_ptr().add(offset).cast(), v);
            }
        }
    }
    #[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
    {
        target.copy_from_slice(source);
        for &idx in added {
            for (a, &w) in target.iter_mut().zip(row(idx)) {
                *a = a.wrapping_add(w);
            }
        }
        for &idx in removed {
            for (a, &w) in target.iter_mut().zip(row(idx)) {
                *a = a.wrapping_sub(w);
            }
        }
    }
}

#[derive(Clone)]
struct AccumulatorEntry {
    accumulator: Accumulator,
    computed: [bool; 2],
    updates: StackVec<UpdateBody, MAX_MOVE_UPDATES>,
    /// Side whose king moved into this position; its perspective must be refreshed.
    king_mover: Option<Color>,
}

impl AccumulatorEntry {
    fn computed_root(accumulator: Accumulator) -> Self {
        Self {
            accumulator,
            computed: [true; 2],
            updates: StackVec::default(),
            king_mover: None,
        }
    }
}

/// One accumulator per position along the current line, computed lazily when evaluated.
#[derive(Clone)]
pub struct AccumulatorStack {
    entries: Vec<AccumulatorEntry>,
    top: usize,
}

impl AccumulatorStack {
    pub fn new(root: Accumulator) -> Self {
        let mut entries = Vec::with_capacity(256);
        entries.push(AccumulatorEntry::computed_root(root));
        Self { entries, top: 0 }
    }

    /// Records the piece changes of a move; nothing is computed until evaluation.
    pub fn push(&mut self, updates: &[UpdateBody], king_mover: Option<Color>) {
        self.top += 1;
        if self.entries.len() == self.top {
            self.entries
                .push(AccumulatorEntry::computed_root(Accumulator::default()));
        }
        let entry = &mut self.entries[self.top];
        entry.computed = [false; 2];
        entry.king_mover = king_mover;
        entry.updates.clear();
        for &update in updates {
            entry.updates.push(update);
        }
    }

    pub fn pop(&mut self) {
        self.top = self.top.saturating_sub(1);
    }

    /// The accumulator of the current position, bringing both perspectives up to date.
    pub fn current(&mut self, ctx: &RefreshContext) -> &Accumulator {
        for perspective in [Color::White, Color::Black] {
            self.ensure_computed(perspective, ctx);
        }
        &self.entries[self.top].accumulator
    }

    fn ensure_computed(&mut self, perspective: Color, ctx: &RefreshContext) {
        let p = perspective as usize;
        let mut base = self.top;
        while !self.entries[base].computed[p] {
            if self.entries[base].king_mover == Some(perspective) || base == 0 {
                let entry = &mut self.entries[self.top];
                refresh_perspective(
                    perspective_half(&mut entry.accumulator, perspective),
                    perspective,
                    ctx,
                );
                entry.computed[p] = true;
                return;
            }
            base -= 1;
        }

        let Some(model) = MODEL.get() else {
            return;
        };
        let king_sq = ctx.king_squares[p];
        for index in base + 1..=self.top {
            let (before, from_index) = self.entries.split_at_mut(index);
            let previous = &mut before[index - 1].accumulator;
            let entry = &mut from_index[0];
            let (added, removed) = changed_features(perspective, king_sq, entry.updates.as_slice());
            apply_features(
                perspective_half(previous, perspective),
                perspective_half(&mut entry.accumulator, perspective),
                &model.ft_weights,
                added.as_slice(),
                removed.as_slice(),
            );
            entry.computed[p] = true;
        }
    }
}

pub fn evaluate(board: &mut Board) -> i32 {
    let Some(model) = MODEL.get() else {
        return 0;
    };

    let turn = board.turn;
    let accumulator = board.accumulator();
    let (acc_us, acc_them) = if turn == Color::White {
        (&accumulator.white, &accumulator.black)
    } else {
        (&accumulator.black, &accumulator.white)
    };

    let mut transformed = AlignedBytes::<HL1_INPUT_DIM>::default();
    clipped_relu_i16(acc_us, &mut transformed.0[..NNUE_FEATURE_SIZE]);
    clipped_relu_i16(acc_them, &mut transformed.0[NNUE_FEATURE_SIZE..]);

    let mut hidden1 = AlignedBytes::<HL1_OUTPUT_DIM>::default();
    affine_clipped_relu(
        &transformed.0,
        &model.hl1_weights,
        &model.hl1_biases,
        &mut hidden1.0,
    );
    let mut hidden2 = AlignedBytes::<HL2_OUTPUT_DIM>::default();
    affine_clipped_relu(
        &hidden1.0,
        &model.hl2_weights,
        &model.hl2_biases,
        &mut hidden2.0,
    );

    let out = model.out_bias + dot_u8_i8(&hidden2.0, &model.out_weights);
    nn_value_to_centipawn(out)
}

#[repr(C, align(32))]
struct AlignedBytes<const N: usize>([u8; N]);

impl<const N: usize> Default for AlignedBytes<N> {
    fn default() -> Self {
        Self([0; N])
    }
}

/// Feature-transformer activation: clamp each accumulator lane to 0..=127.
#[inline(always)]
fn clipped_relu_i16(input: &[i16], output: &mut [u8]) {
    for (out, &value) in output.iter_mut().zip(input) {
        *out = value.clamp(0, 127) as u8;
    }
}

/// Dense layer with i8 weights (row per output) followed by the scaled clipped ReLU.
#[inline(always)]
fn affine_clipped_relu(input: &[u8], weights: &[i8], biases: &[i32], output: &mut [u8]) {
    let in_dim = input.len();
    for (j, out) in output.iter_mut().enumerate() {
        let row = &weights[j * in_dim..(j + 1) * in_dim];
        let sum = biases[j] + dot_u8_i8(input, row);
        *out = (sum >> WEIGHT_SCALE_BITS).clamp(0, 127) as u8;
    }
}

/// HalfKP orients black's view by rotating the board, not by flipping ranks.
#[inline(always)]
const fn rotate_180(sq: usize) -> usize {
    sq ^ 63
}

#[inline(always)]
fn make_halfkp_index(
    is_white_pov: bool,
    king_sq: usize,
    sq: usize,
    piece: Piece,
    piece_color: Color,
) -> usize {
    let oriented_sq = if is_white_pov { sq } else { rotate_180(sq) };
    let king_oriented = if is_white_pov {
        king_sq
    } else {
        rotate_180(king_sq)
    };

    let color_is_pov = (piece_color == Color::White) == is_white_pov;
    let color_offset = if color_is_pov { 0 } else { 1 };

    let piece_offset = match piece.kind().unwrap() {
        PieceKind::Pawn => 0,
        PieceKind::Knight => 1,
        PieceKind::Bishop => 2,
        PieceKind::Rook => 3,
        PieceKind::Queen => 4,
        PieceKind::King => 5,
    };

    let piece_idx = (piece_offset * 2 + color_offset) * SQUARE_NB + 1;
    oriented_sq + piece_idx + 641 * king_oriented
}

/// Dot product of activations (0..=127) and weights; exact for any length.
#[inline(always)]
fn dot_u8_i8(input: &[u8], weights: &[i8]) -> i32 {
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    {
        dot_u8_i8_avx2(input, weights)
    }
    #[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
    {
        dot_u8_i8_scalar(input, weights)
    }
}

#[cfg_attr(all(target_arch = "x86_64", target_feature = "avx2"), allow(dead_code))]
fn dot_u8_i8_scalar(input: &[u8], weights: &[i8]) -> i32 {
    input
        .iter()
        .zip(weights)
        .map(|(&x, &w)| i32::from(x) * i32::from(w))
        .sum()
}

/// Multiplies 32 u8 activations by 32 i8 weights and adds groups of four to `acc`.
///
/// With AVX-VNNI this is one exact `dpbusd`. Otherwise `maddubs` sums adjacent pairs into
/// i16; with activations <= 127 a pair lies within ±32_512, so it never saturates.
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
#[inline(always)]
fn dpbusd(acc: __m256i, x: __m256i, w: __m256i) -> __m256i {
    #[cfg(target_feature = "avxvnni")]
    {
        // SAFETY: AVX-VNNI is enabled at compile time.
        unsafe { _mm256_dpbusd_avx_epi32(acc, x, w) }
    }
    #[cfg(not(target_feature = "avxvnni"))]
    {
        // SAFETY: AVX2 is enabled at compile time.
        unsafe {
            let pairs = _mm256_maddubs_epi16(x, w);
            _mm256_add_epi32(acc, _mm256_madd_epi16(pairs, _mm256_set1_epi16(1)))
        }
    }
}

#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
#[inline(always)]
fn dot_u8_i8_avx2(input: &[u8], weights: &[i8]) -> i32 {
    const LANES: usize = 32;
    let len = input.len().min(weights.len());
    let chunks = len / LANES;
    // SAFETY: AVX2 is enabled at compile time; every load reads LANES bytes inside
    // both slices because offset + LANES <= chunks * LANES <= len.
    let mut sum = unsafe {
        let mut acc = _mm256_setzero_si256();
        for chunk in 0..chunks {
            let offset = chunk * LANES;
            let x = _mm256_loadu_si256(input.as_ptr().add(offset).cast());
            let w = _mm256_loadu_si256(weights.as_ptr().add(offset).cast());
            acc = dpbusd(acc, x, w);
        }
        let lo = _mm256_castsi256_si128(acc);
        let hi = _mm256_extracti128_si256(acc, 1);
        let quad = _mm_add_epi32(lo, hi);
        let pair = _mm_add_epi32(quad, _mm_shuffle_epi32(quad, 0b01_00_11_10));
        let single = _mm_add_epi32(pair, _mm_shuffle_epi32(pair, 0b10_11_00_01));
        _mm_cvtsi128_si32(single)
    };
    for i in chunks * LANES..len {
        sum += i32::from(input[i]) * i32::from(weights[i]);
    }
    sum
}

#[inline]
/// Network output to centipawns: Stockfish 12 divides by `FV_SCALE` (16) to get internal
/// units, in which an endgame pawn is worth 208.
fn nn_value_to_centipawn(nn_value: i32) -> i32 {
    const FV_SCALE: i32 = 16;
    const PAWN_VALUE_EG: i32 = 208;
    nn_value / FV_SCALE * 100 / PAWN_VALUE_EG
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn ensure_model_loaded() {
        match init() {
            Ok(()) | Err(NnueError::AlreadyInitialized) => {}
            Err(e) => panic!("failed to load NNUE: {e}"),
        }
    }

    /// Rotates the board 180 degrees and swaps colors; castling and en passant are dropped.
    fn rotated_color_swapped_fen(board: &Board) -> String {
        let mut swapped = [Piece::Empty; 64];
        for sq in 0..64 {
            let piece = board.piece_on[sq];
            if let (Some(kind), Some(color)) = (piece.kind(), piece.color()) {
                swapped[sq ^ 63] = Piece::from_kind(kind, color.other());
            }
        }
        let mut rows = Vec::with_capacity(8);
        for rank in (0..8).rev() {
            let mut row = String::new();
            let mut empty_run = 0;
            for file in 0..8 {
                let piece = swapped[rank * 8 + file];
                if piece.is_empty() {
                    empty_run += 1;
                    continue;
                }
                if empty_run > 0 {
                    row.push_str(&empty_run.to_string());
                    empty_run = 0;
                }
                row.push_str(&piece.to_string());
            }
            if empty_run > 0 {
                row.push_str(&empty_run.to_string());
            }
            rows.push(row);
        }
        let side = if board.turn == Color::White { "b" } else { "w" };
        format!("{} {side} - - 0 1", rows.join("/"))
    }

    fn assert_accumulator_matches_refresh(board: &mut Board, context: &str) {
        let fresh = refresh_accumulator(board);
        let current = *board.accumulator();
        assert_eq!(current.white, fresh.white, "white half: {context}");
        assert_eq!(current.black, fresh.black, "black half: {context}");
    }

    /// Walks every legal move to `depth`, checking the accumulator before and after each.
    fn walk_and_check(board: &mut Board, depth: usize) {
        if depth == 0 {
            return;
        }
        let mut moves = crate::types::MoveList::new();
        board.generate_legal_moves(&mut moves);
        for &m in moves.as_slice() {
            let before = board.to_fen();
            let undo = board.make_move(m);
            assert_accumulator_matches_refresh(board, &format!("{before} after {m:?}"));
            walk_and_check(board, depth - 1);
            board.unmake_move(m, undo);
            assert_accumulator_matches_refresh(board, &format!("{before} after undoing {m:?}"));
        }
    }

    #[test]
    fn incremental_accumulator_matches_refresh_through_special_moves() {
        ensure_model_loaded();
        let fens = [
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1",
            "rnbqkbnr/ppp1pppp/8/3pP3/8/8/PPPP1PPP/RNBQKBNR w KQkq d6 0 3",
        ];
        for fen in fens {
            let mut board = Board::from_fen(fen).unwrap();
            walk_and_check(&mut board, 2);
        }
    }

    /// Makes moves to `depth` without evaluating in between, checking only the leaves,
    /// so each check computes a chain of `depth` pending updates at once.
    fn walk_and_check_leaves(board: &mut Board, depth: usize) {
        if depth == 0 {
            assert_accumulator_matches_refresh(board, &board.to_fen());
            return;
        }
        let mut moves = crate::types::MoveList::new();
        board.generate_legal_moves(&mut moves);
        for &m in moves.as_slice() {
            let undo = board.make_move(m);
            walk_and_check_leaves(board, depth - 1);
            board.unmake_move(m, undo);
        }
    }

    #[test]
    fn lazily_computed_accumulator_chains_match_refresh() {
        ensure_model_loaded();
        let fens = [
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
        ];
        for fen in fens {
            let mut board = Board::from_fen(fen).unwrap();
            walk_and_check_leaves(&mut board, 3);
        }
    }

    #[test]
    fn simd_dot_product_matches_scalar_at_extreme_values() {
        let lengths = [32usize, 512, 37];
        for len in lengths {
            let input: Vec<u8> = (0..len)
                .map(|i| if i % 3 == 0 { 127 } else { (i * 7 % 128) as u8 })
                .collect();
            let weights: Vec<i8> = (0..len)
                .map(|i| match i % 4 {
                    0 => -128,
                    1 => 127,
                    _ => ((i * 13) % 256) as u8 as i8,
                })
                .collect();
            assert_eq!(
                dot_u8_i8(&input, &weights),
                dot_u8_i8_scalar(&input, &weights),
                "len {len}"
            );
        }
    }

    #[test]
    fn eval_is_invariant_under_rotation_and_color_swap() {
        ensure_model_loaded();
        let fens = [
            "r1bq1rk1/ppp1nppp/4n3/3p3Q/3P4/1BP1B3/PP1N2PP/R4RK1 w - - 1 16",
            "4k2r/1pb2ppp/1p2p3/1R1p4/3P4/2r1PN2/P4PPP/1R4K1 b - - 3 22",
            "6k1/6p1/P6p/r1N5/5p2/7P/1b3PP1/4R1K1 w - - 0 1",
            "2K5/p7/7P/5pR1/8/5k2/r7/8 w - - 0 1",
        ];
        for fen in fens {
            let mut board = Board::from_fen(fen).unwrap();
            let mut mirrored = Board::from_fen(&rotated_color_swapped_fen(&board)).unwrap();
            assert_eq!(
                evaluate(&mut board),
                evaluate(&mut mirrored),
                "asymmetric eval for {fen}"
            );
        }
    }
}
