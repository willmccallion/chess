use crate::board::Board;
use crate::types::{Accumulator, Color, NNUE_FEATURE_SIZE, Piece, PieceKind, UpdateBody};
use byteorder::{LittleEndian, ReadBytesExt};
use once_cell::sync::OnceCell;
use std::error::Error;
use std::fmt;
use std::io::{BufReader, Cursor, Read, Seek};

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;

const SQUARE_NB: usize = 64;
const FT_INPUT_DIM: usize = 41024;
const HL1_INPUT_DIM: usize = 512;
const HL1_OUTPUT_DIM: usize = 32;
const HL2_OUTPUT_DIM: usize = 32;

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

/// Computes the accumulator from scratch. Used when loading FEN or when King moves.
pub fn refresh_accumulator(board: &Board) -> Accumulator {
    let Some(model) = MODEL.get() else {
        return Accumulator::default();
    };

    let mut acc = Accumulator::default();

    acc.white.copy_from_slice(&model.ft_biases);
    acc.black.copy_from_slice(&model.ft_biases);

    let wk_sq = board.king_square(Color::White) as usize;
    let bk_sq = board.king_square(Color::Black) as usize;

    let mut occupied = board.all_pieces;
    occupied &= !board.piece_bb[Piece::WK.index()];
    occupied &= !board.piece_bb[Piece::BK.index()];

    while occupied != 0 {
        let sq = occupied.trailing_zeros() as usize;
        occupied &= occupied - 1;

        let piece = board.piece_on[sq];
        let color = piece.color().unwrap();

        let idx_w = make_halfkp_index(true, wk_sq, sq, piece, color);
        add_weight(&mut acc.white, &model.ft_weights, idx_w);

        let idx_b = make_halfkp_index(false, bk_sq, sq, piece, color);
        add_weight(&mut acc.black, &model.ft_weights, idx_b);
    }

    acc
}

/// Incrementally updates the accumulator based on a list of changes.
pub fn update_accumulator(
    acc: &mut Accumulator,
    wk_sq: usize,
    bk_sq: usize,
    updates: &[UpdateBody],
) {
    let Some(model) = MODEL.get() else {
        return;
    };

    for u in updates {
        let color = u.piece.color().unwrap();

        let idx_w = make_halfkp_index(true, wk_sq, u.sq, u.piece, color);
        if u.add {
            add_weight(&mut acc.white, &model.ft_weights, idx_w);
        } else {
            sub_weight(&mut acc.white, &model.ft_weights, idx_w);
        }

        let idx_b = make_halfkp_index(false, bk_sq, u.sq, u.piece, color);
        if u.add {
            add_weight(&mut acc.black, &model.ft_weights, idx_b);
        } else {
            sub_weight(&mut acc.black, &model.ft_weights, idx_b);
        }
    }
}

pub fn evaluate(board: &Board) -> i32 {
    let Some(model) = MODEL.get() else {
        return 0;
    };

    let is_white = board.turn == Color::White;

    let mut ft_us = [0i32; NNUE_FEATURE_SIZE];
    let mut ft_them = [0i32; NNUE_FEATURE_SIZE];

    let (acc_us, acc_them) = if is_white {
        (&board.accumulator.white, &board.accumulator.black)
    } else {
        (&board.accumulator.black, &board.accumulator.white)
    };

    for i in 0..NNUE_FEATURE_SIZE {
        ft_us[i] = acc_us[i].clamp(0, 127) as i32;
        ft_them[i] = acc_them[i].clamp(0, 127) as i32;
    }

    let mut concat = [0i32; HL1_INPUT_DIM];
    concat[..NNUE_FEATURE_SIZE].copy_from_slice(&ft_us);
    concat[NNUE_FEATURE_SIZE..].copy_from_slice(&ft_them);

    let hl1 = dense_layer(
        &concat,
        &model.hl1_weights,
        &model.hl1_biases,
        HL1_INPUT_DIM,
        HL1_OUTPUT_DIM,
    );
    let hl2 = dense_layer(
        &hl1,
        &model.hl2_weights,
        &model.hl2_biases,
        HL1_OUTPUT_DIM,
        HL2_OUTPUT_DIM,
    );
    let out = dense_output(&hl2, &model.out_weights, model.out_bias);

    nn_value_to_centipawn(out)
}

#[inline(always)]
fn make_halfkp_index(
    is_white_pov: bool,
    king_sq: usize,
    sq: usize,
    piece: Piece,
    piece_color: Color,
) -> usize {
    let oriented_sq = if is_white_pov { sq } else { sq ^ 56 };
    let king_oriented = if is_white_pov { king_sq } else { king_sq ^ 56 };

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

#[inline(always)]
fn add_weight(acc: &mut [i16], weights: &[i16], idx: usize) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        add_weight_avx2(acc, weights, idx)
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        add_weight_neon(acc, weights, idx)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let offset = idx * NNUE_FEATURE_SIZE;
        for i in 0..NNUE_FEATURE_SIZE {
            acc[i] = acc[i].wrapping_add(weights[offset + i]);
        }
    }
}

#[inline(always)]
fn sub_weight(acc: &mut [i16], weights: &[i16], idx: usize) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        sub_weight_avx2(acc, weights, idx)
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        sub_weight_neon(acc, weights, idx)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let offset = idx * NNUE_FEATURE_SIZE;
        for i in 0..NNUE_FEATURE_SIZE {
            acc[i] = acc[i].wrapping_sub(weights[offset + i]);
        }
    }
}

#[inline(always)]
fn dot_product(input: &[i32], weights: &[i8]) -> i32 {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        dot_product_avx2(input, weights)
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        dot_product_neon(input, weights)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        input
            .iter()
            .zip(weights.iter())
            .map(|(&x, &w)| x * (w as i32))
            .sum()
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn add_weight_avx2(acc: &mut [i16], weights: &[i16], idx: usize) {
    let offset = idx * NNUE_FEATURE_SIZE;
    let mut i = 0;
    while i < NNUE_FEATURE_SIZE {
        unsafe {
            let acc_ptr = acc.as_mut_ptr().add(i);
            let wt_ptr = weights.as_ptr().add(offset + i);
            let acc_vec = _mm256_load_si256(acc_ptr as *const __m256i);
            let wt_vec = _mm256_loadu_si256(wt_ptr as *const __m256i);
            let res = _mm256_add_epi16(acc_vec, wt_vec);
            _mm256_store_si256(acc_ptr as *mut __m256i, res);
        }
        i += 16;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn sub_weight_avx2(acc: &mut [i16], weights: &[i16], idx: usize) {
    let offset = idx * NNUE_FEATURE_SIZE;
    let mut i = 0;
    while i < NNUE_FEATURE_SIZE {
        unsafe {
            let acc_ptr = acc.as_mut_ptr().add(i);
            let wt_ptr = weights.as_ptr().add(offset + i);
            let acc_vec = _mm256_load_si256(acc_ptr as *const __m256i);
            let wt_vec = _mm256_loadu_si256(wt_ptr as *const __m256i);
            let res = _mm256_sub_epi16(acc_vec, wt_vec);
            _mm256_store_si256(acc_ptr as *mut __m256i, res);
        }
        i += 16;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn dot_product_avx2(input: &[i32], weights: &[i8]) -> i32 {
    let len = input.len();
    let mut i = 0;
    let mut acc = _mm256_setzero_si256();
    while i + 16 <= len {
        unsafe {
            let in_vec1 = _mm256_loadu_si256(input.as_ptr().add(i) as *const __m256i);
            let wt_chunk1 = _mm_loadl_epi64(weights.as_ptr().add(i) as *const __m128i);
            let wt_vec1 = _mm256_cvtepi8_epi32(wt_chunk1);
            let in_vec2 = _mm256_loadu_si256(input.as_ptr().add(i + 8) as *const __m256i);
            let wt_chunk2 = _mm_loadl_epi64(weights.as_ptr().add(i + 8) as *const __m128i);
            let wt_vec2 = _mm256_cvtepi8_epi32(wt_chunk2);
            let prod1 = _mm256_madd_epi16(
                _mm256_packs_epi32(in_vec1, in_vec2),
                _mm256_packs_epi32(wt_vec1, wt_vec2),
            );
            acc = _mm256_add_epi32(acc, prod1);
        }
        i += 16;
    }
    let mut acc_arr = [0i32; 8];
    unsafe {
        _mm256_storeu_si256(acc_arr.as_mut_ptr() as *mut __m256i, acc);
    }
    let mut sum = acc_arr.iter().sum();
    while i < len {
        sum += input[i] * (weights[i] as i32);
        i += 1;
    }
    sum
}

#[cfg(target_arch = "aarch64")]
unsafe fn add_weight_neon(acc: &mut [i16], weights: &[i16], idx: usize) {
    let offset = idx * NNUE_FEATURE_SIZE;
    let mut i = 0;
    while i < NNUE_FEATURE_SIZE {
        let acc_ptr = acc.as_mut_ptr().add(i);
        let wt_ptr = weights.as_ptr().add(offset + i);

        let a0 = vld1q_s16(acc_ptr);
        let a1 = vld1q_s16(acc_ptr.add(8));
        let w0 = vld1q_s16(wt_ptr);
        let w1 = vld1q_s16(wt_ptr.add(8));

        vst1q_s16(acc_ptr, vaddq_s16(a0, w0));
        vst1q_s16(acc_ptr.add(8), vaddq_s16(a1, w1));

        i += 16;
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn sub_weight_neon(acc: &mut [i16], weights: &[i16], idx: usize) {
    let offset = idx * NNUE_FEATURE_SIZE;
    let mut i = 0;
    while i < NNUE_FEATURE_SIZE {
        let acc_ptr = acc.as_mut_ptr().add(i);
        let wt_ptr = weights.as_ptr().add(offset + i);

        let a0 = vld1q_s16(acc_ptr);
        let a1 = vld1q_s16(acc_ptr.add(8));
        let w0 = vld1q_s16(wt_ptr);
        let w1 = vld1q_s16(wt_ptr.add(8));

        vst1q_s16(acc_ptr, vsubq_s16(a0, w0));
        vst1q_s16(acc_ptr.add(8), vsubq_s16(a1, w1));

        i += 16;
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn dot_product_neon(input: &[i32], weights: &[i8]) -> i32 {
    let len = input.len();
    let mut i = 0;
    let mut sum0 = vdupq_n_s32(0);
    let mut sum1 = vdupq_n_s32(0);

    while i + 8 <= len {
        let w_ptr = weights.as_ptr().add(i);
        let w_8 = vld1_s8(w_ptr);

        let w_16 = vmovl_s8(w_8);

        let w_32_low = vmovl_s16(vget_low_s16(w_16));
        let w_32_high = vmovl_s16(vget_high_s16(w_16));

        let in_ptr = input.as_ptr().add(i);
        let in_0 = vld1q_s32(in_ptr);
        let in_1 = vld1q_s32(in_ptr.add(4));

        sum0 = vmlaq_s32(sum0, in_0, w_32_low);
        sum1 = vmlaq_s32(sum1, in_1, w_32_high);

        i += 8;
    }

    let sum = vaddq_s32(sum0, sum1);
    let mut result = vaddvq_s32(sum);

    while i < len {
        result += input[i] * (weights[i] as i32);
        i += 1;
    }
    result
}

#[inline]
fn dense_layer(
    input: &[i32],
    weights: &[i8],
    biases: &[i32],
    in_dim: usize,
    out_dim: usize,
) -> [i32; HL1_OUTPUT_DIM] {
    let mut out = [0i32; HL1_OUTPUT_DIM];
    for j in 0..out_dim {
        let weight_slice = &weights[j * in_dim..(j + 1) * in_dim];
        let sum = biases[j] + dot_product(input, weight_slice);
        out[j] = nnue_relu(sum);
    }
    out
}

#[inline]
fn dense_output(input: &[i32], weights: &[i8], bias: i32) -> i32 {
    bias + dot_product(input, weights)
}

#[inline]
fn nnue_relu(x: i32) -> i32 {
    if x < 0 {
        0
    } else {
        let y = x / 64;
        if y > 127 { 127 } else { y }
    }
}

#[inline]
fn nn_value_to_centipawn(nn_value: i32) -> i32 {
    let v = nn_value / 8;
    (v * 100) / 208
}
