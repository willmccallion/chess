//! SIMD kernels shared by the network layers.

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

/// Dot product of activations (0..=127) and weights; exact for any length.
#[inline(always)]
pub fn dot_u8_i8(input: &[u8], weights: &[i8]) -> i32 {
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
pub(crate) fn dot_u8_i8_scalar(input: &[u8], weights: &[i8]) -> i32 {
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

/// A weight matrix whose rows are `N` lanes wide, stored as i16 or i8.
pub trait WeightRows<const N: usize> {
    fn row_count(&self) -> usize;
    /// Adds (or subtracts) row `row` to `acc` without SIMD.
    fn apply_scalar(&self, row: usize, acc: &mut [i16; N], subtract: bool);
    /// # Safety
    /// AVX2 must be available and `row < self.row_count()`.
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    unsafe fn load(&self, row: usize, lane: usize) -> __m256i;
}

impl<const N: usize> WeightRows<N> for [i16] {
    fn row_count(&self) -> usize {
        self.len() / N
    }

    fn apply_scalar(&self, row: usize, acc: &mut [i16; N], subtract: bool) {
        for (value, &w) in acc.iter_mut().zip(&self[row * N..(row + 1) * N]) {
            *value = if subtract {
                value.wrapping_sub(w)
            } else {
                value.wrapping_add(w)
            };
        }
    }

    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    #[inline(always)]
    unsafe fn load(&self, row: usize, lane: usize) -> __m256i {
        // SAFETY: row < row_count and lane + 16 <= N keep the 16 lanes inside the slice.
        unsafe { _mm256_loadu_si256(self.as_ptr().add(row * N + lane).cast()) }
    }
}

impl<const N: usize> WeightRows<N> for [i8] {
    fn row_count(&self) -> usize {
        self.len() / N
    }

    fn apply_scalar(&self, row: usize, acc: &mut [i16; N], subtract: bool) {
        for (value, &w) in acc.iter_mut().zip(&self[row * N..(row + 1) * N]) {
            let w = i16::from(w);
            *value = if subtract {
                value.wrapping_sub(w)
            } else {
                value.wrapping_add(w)
            };
        }
    }

    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    #[inline(always)]
    unsafe fn load(&self, row: usize, lane: usize) -> __m256i {
        // SAFETY: row < row_count and lane + 16 <= N keep the 16 bytes inside the slice.
        unsafe {
            let bytes = _mm_loadu_si128(self.as_ptr().add(row * N + lane).cast());
            _mm256_cvtepi8_epi16(bytes)
        }
    }
}

/// `acc += Σ rows[adds] − Σ rows[subs]`, lane-wise with wrapping, in one pass over `acc`.
/// Rows past the end of the matrix are ignored; feature indices never produce them.
pub fn update_rows<const N: usize, W: WeightRows<N> + ?Sized>(
    acc: &mut [i16; N],
    rows: &W,
    adds: &[usize],
    subs: &[usize],
) {
    let count = rows.row_count();
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    if N.is_multiple_of(TILE_LANES) && adds.iter().chain(subs).all(|&row| row < count) {
        // SAFETY: AVX2 is enabled at compile time, every row is < row_count, and N is a
        // multiple of the tile width; reading and writing the same array is allowed.
        let base = acc.as_mut_ptr();
        unsafe { update_rows_avx2(base.cast_const(), base, rows, adds, subs) };
        return;
    }
    for &row in adds.iter().filter(|&&r| r < count) {
        rows.apply_scalar(row, acc, false);
    }
    for &row in subs.iter().filter(|&&r| r < count) {
        rows.apply_scalar(row, acc, true);
    }
}

/// `dst = src + Σ rows[adds] − Σ rows[subs]`; like `update_rows` without a separate copy.
pub fn update_rows_from<const N: usize, W: WeightRows<N> + ?Sized>(
    src: &[i16; N],
    dst: &mut [i16; N],
    rows: &W,
    adds: &[usize],
    subs: &[usize],
) {
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    if N.is_multiple_of(TILE_LANES) && adds.iter().chain(subs).all(|&row| row < rows.row_count()) {
        // SAFETY: AVX2 is enabled at compile time, every row is < row_count, N is a multiple
        // of the tile width, and src and dst are distinct N-lane arrays.
        unsafe { update_rows_avx2(src.as_ptr(), dst.as_mut_ptr(), rows, adds, subs) };
        return;
    }
    dst.copy_from_slice(src);
    update_rows(dst, rows, adds, subs);
}

const TILE_REGS: usize = 8;
const REG_LANES: usize = 16;
const TILE_LANES: usize = TILE_REGS * REG_LANES;

/// # Safety
/// AVX2 must be available, every row must be < `rows.row_count()`, `N` must be a multiple of
/// `TILE_LANES`, and `src` and `dst` must each point to `N` lanes (they may be the same).
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
#[inline(always)]
unsafe fn update_rows_avx2<const N: usize, W: WeightRows<N> + ?Sized>(
    src: *const i16,
    dst: *mut i16,
    rows: &W,
    adds: &[usize],
    subs: &[usize],
) {
    for tile in (0..N).step_by(TILE_LANES) {
        // SAFETY: tile + TILE_LANES <= N keeps every 16-lane access inside src and dst; each
        // tile is fully loaded before it is stored, so src == dst is fine; the caller
        // guarantees AVX2 and in-range rows for `load`.
        unsafe {
            let mut regs = [_mm256_setzero_si256(); TILE_REGS];
            for (k, reg) in regs.iter_mut().enumerate() {
                *reg = _mm256_loadu_si256(src.add(tile + k * REG_LANES).cast());
            }
            for &row in adds {
                for (k, reg) in regs.iter_mut().enumerate() {
                    *reg = _mm256_add_epi16(*reg, rows.load(row, tile + k * REG_LANES));
                }
            }
            for &row in subs {
                for (k, reg) in regs.iter_mut().enumerate() {
                    *reg = _mm256_sub_epi16(*reg, rows.load(row, tile + k * REG_LANES));
                }
            }
            for (k, reg) in regs.iter().enumerate() {
                _mm256_storeu_si256(dst.add(tile + k * REG_LANES).cast(), *reg);
            }
        }
    }
}

/// Pairwise product of clipped first-layer halves: `out[j] = clip(a[j]) * clip(b[j]) / 512`,
/// where each half is the piece-square sum plus the optional threat sum (wrapping).
pub fn pairwise_clipped_product(
    first: &[i16],
    second: &[i16],
    threats: Option<(&[i16], &[i16])>,
    clip: i16,
    out: &mut [u8],
) {
    let len = out.len().min(first.len()).min(second.len());
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    if len.is_multiple_of(32) && threats.is_none_or(|(a, b)| a.len() >= len && b.len() >= len) {
        // SAFETY: AVX2 is enabled at compile time and every slice holds at least `len`
        // elements, a multiple of 32.
        unsafe { pairwise_clipped_product_avx2(first, second, threats, clip, &mut out[..len]) };
        return;
    }
    for j in 0..len {
        let (mut a, mut b) = (first[j], second[j]);
        if let Some((ta, tb)) = threats {
            a = a.wrapping_add(ta[j]);
            b = b.wrapping_add(tb[j]);
        }
        let a = i32::from(a.clamp(0, clip));
        let b = i32::from(b.clamp(0, clip));
        out[j] = ((a * b) / 512) as u8;
    }
}

/// # Safety
/// AVX2 must be available; `first`, `second` and both threat slices must hold at least
/// `out.len()` elements, and `out.len()` must be a multiple of 32.
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
unsafe fn pairwise_clipped_product_avx2(
    first: &[i16],
    second: &[i16],
    threats: Option<(&[i16], &[i16])>,
    clip: i16,
    out: &mut [u8],
) {
    // SAFETY: every load and store covers lanes j..j + 32 with j + 32 <= out.len(), which
    // the caller guarantees is within each slice.
    unsafe {
        let zero = _mm256_setzero_si256();
        let max = _mm256_set1_epi16(clip);
        let load = |slice: &[i16], j: usize| _mm256_loadu_si256(slice.as_ptr().add(j).cast());
        for j in (0..out.len()).step_by(32) {
            let mut a0 = load(first, j);
            let mut a1 = load(first, j + 16);
            let mut b0 = load(second, j);
            let mut b1 = load(second, j + 16);
            if let Some((ta, tb)) = threats {
                a0 = _mm256_add_epi16(a0, load(ta, j));
                a1 = _mm256_add_epi16(a1, load(ta, j + 16));
                b0 = _mm256_add_epi16(b0, load(tb, j));
                b1 = _mm256_add_epi16(b1, load(tb, j + 16));
            }
            // clip(a) << 7 times min(b, clip), keeping the high half, is clip(a) * clip(b) / 512
            // whenever b >= 0; for b < 0 it is negative and packus turns it into 0.
            let a0 = _mm256_slli_epi16(_mm256_max_epi16(_mm256_min_epi16(a0, max), zero), 7);
            let a1 = _mm256_slli_epi16(_mm256_max_epi16(_mm256_min_epi16(a1, max), zero), 7);
            let p0 = _mm256_mulhi_epi16(a0, _mm256_min_epi16(b0, max));
            let p1 = _mm256_mulhi_epi16(a1, _mm256_min_epi16(b1, max));
            let packed = _mm256_packus_epi16(p0, p1);
            let ordered = _mm256_permute4x64_epi64(packed, 0b11_01_10_00);
            _mm256_storeu_si256(out.as_mut_ptr().add(j).cast(), ordered);
        }
    }
}

/// First-layer weights reordered for sparse input: `[input chunk of 4][output][4]`.
pub fn scramble_for_sparse(weights: &[i8], inputs: usize, outputs: usize) -> Vec<i8> {
    let mut scrambled = vec![0i8; weights.len()];
    for output in 0..outputs {
        for input in 0..inputs {
            let chunk = input / 4;
            scrambled[(chunk * outputs + output) * 4 + input % 4] =
                weights[output * inputs + input];
        }
    }
    scrambled
}

/// `biases + W x` for 16 outputs, skipping all-zero groups of four inputs. `scrambled` comes
/// from `scramble_for_sparse`; inputs must be at most 127.
pub fn sparse_affine_16(input: &[u8], scrambled: &[i8], biases: &[i32; 16]) -> [i32; 16] {
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    if input.len().is_multiple_of(32)
        && input.len() <= SPARSE_MAX_INPUTS
        && scrambled.len() >= input.len() * 16
    {
        // SAFETY: AVX2 is enabled at compile time, the input length is a multiple of 32 and
        // the weights cover 16 outputs for every input.
        return unsafe { sparse_affine_16_avx2(input, scrambled, biases) };
    }
    let mut out = *biases;
    for (chunk, group) in input.as_chunks::<4>().0.iter().enumerate() {
        if *group == [0; 4] {
            continue;
        }
        for (o, sum) in out.iter_mut().enumerate() {
            let w = &scrambled[(chunk * 16 + o) * 4..(chunk * 16 + o) * 4 + 4];
            *sum += group
                .iter()
                .zip(w)
                .map(|(&x, &w)| i32::from(x) * i32::from(w))
                .sum::<i32>();
        }
    }
    out
}

const SPARSE_MAX_INPUTS: usize = 1024;
const SPARSE_MAX_CHUNKS: usize = SPARSE_MAX_INPUTS / 4;

/// For every 8-bit mask, the positions of its set bits, padded with zeros.
const fn set_bit_positions() -> [[u16; 8]; 256] {
    let mut table = [[0u16; 8]; 256];
    let mut mask = 0;
    while mask < 256 {
        let mut count = 0;
        let mut bit = 0;
        while bit < 8 {
            if mask & (1 << bit) != 0 {
                table[mask][count] = bit as u16;
                count += 1;
            }
            bit += 1;
        }
        mask += 1;
    }
    table
}

static SET_BIT_POSITIONS: [[u16; 8]; 256] = set_bit_positions();

/// # Safety
/// AVX2 must be available, `input.len()` a multiple of 32 and at most `SPARSE_MAX_INPUTS`,
/// and `scrambled.len()` at least `16 * input.len()`.
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
unsafe fn sparse_affine_16_avx2(input: &[u8], scrambled: &[i8], biases: &[i32; 16]) -> [i32; 16] {
    // Each block writes 8 entries at `count`, so keep 8 spare slots past the last chunk.
    let mut nonzero = [0u16; SPARSE_MAX_CHUNKS + 8];
    let mut count = 0usize;
    // SAFETY: block loads stay inside `input`; `count` is at most the number of chunks
    // seen so far (<= SPARSE_MAX_CHUNKS), so each 8-entry store fits in `nonzero`; every
    // nonzero chunk c reads weights c * 64 .. c * 64 + 64 inside `scrambled`; the caller
    // guarantees AVX2.
    unsafe {
        let zero = _mm256_setzero_si256();
        for block in 0..input.len() / 32 {
            let v = _mm256_loadu_si256(input.as_ptr().add(block * 32).cast());
            let zero_lanes = _mm256_movemask_ps(_mm256_castsi256_ps(_mm256_cmpeq_epi32(v, zero)));
            let mask = !zero_lanes as usize & 0xFF;
            let positions = _mm_loadu_si128(SET_BIT_POSITIONS[mask].as_ptr().cast());
            let chunks = _mm_add_epi16(_mm_set1_epi16((block * 8) as i16), positions);
            _mm_storeu_si128(nonzero.as_mut_ptr().add(count).cast(), chunks);
            count += mask.count_ones() as usize;
        }
        let mut low = _mm256_loadu_si256(biases.as_ptr().cast());
        let mut high = _mm256_loadu_si256(biases.as_ptr().add(8).cast());
        for &chunk in &nonzero[..count] {
            let chunk = usize::from(chunk);
            let x = _mm256_set1_epi32(input.as_ptr().add(chunk * 4).cast::<i32>().read_unaligned());
            let w = scrambled.as_ptr().add(chunk * 64);
            low = dpbusd(low, x, _mm256_loadu_si256(w.cast()));
            high = dpbusd(high, x, _mm256_loadu_si256(w.add(32).cast()));
        }
        let mut out = [0i32; 16];
        _mm256_storeu_si256(out.as_mut_ptr().cast(), low);
        _mm256_storeu_si256(out.as_mut_ptr().add(8).cast(), high);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn fused_row_updates_match_one_row_at_a_time() {
        const N: usize = 256;
        let wide: Vec<i16> = (0..N * 6)
            .map(|i| ((i * 7919) % 65_536) as u16 as i16)
            .collect();
        let narrow: Vec<i8> = (0..N * 6).map(|i| ((i * 31) % 256) as u8 as i8).collect();
        let start: [i16; N] = std::array::from_fn(|i| (i as i16).wrapping_mul(97));
        let (adds, subs) = ([0usize, 3, 5], [1usize, 4]);

        let mut fused = start;
        update_rows::<N, [i16]>(&mut fused, &wide, &adds, &subs);
        let mut stepwise = start;
        for &r in &adds {
            WeightRows::<N>::apply_scalar(wide.as_slice(), r, &mut stepwise, false);
        }
        for &r in &subs {
            WeightRows::<N>::apply_scalar(wide.as_slice(), r, &mut stepwise, true);
        }
        assert_eq!(fused, stepwise);

        let mut fused = start;
        update_rows::<N, [i8]>(&mut fused, &narrow, &adds, &subs);
        let mut stepwise = start;
        for &r in &adds {
            WeightRows::<N>::apply_scalar(narrow.as_slice(), r, &mut stepwise, false);
        }
        for &r in &subs {
            WeightRows::<N>::apply_scalar(narrow.as_slice(), r, &mut stepwise, true);
        }
        assert_eq!(fused, stepwise);
    }

    #[test]
    fn pairwise_product_matches_scalar_reference() {
        let first: Vec<i16> = (0..64).map(|i| (i * 37 % 600) as i16 - 200).collect();
        let second: Vec<i16> = (0..64).map(|i| (i * 53 % 700) as i16 - 250).collect();
        let ta: Vec<i16> = (0..64).map(|i| (i * 11 % 90) as i16 - 45).collect();
        let tb: Vec<i16> = (0..64).map(|i| (i * 17 % 80) as i16 - 40).collect();
        for (threats, clip) in [(None, 254i16), (Some((ta.as_slice(), tb.as_slice())), 255)] {
            let mut fast = [0u8; 64];
            pairwise_clipped_product(&first, &second, threats, clip, &mut fast);
            for j in 0..64 {
                let (mut a, mut b) = (first[j], second[j]);
                if let Some((ta, tb)) = threats {
                    a = a.wrapping_add(ta[j]);
                    b = b.wrapping_add(tb[j]);
                }
                let expected =
                    (i32::from(a.clamp(0, clip)) * i32::from(b.clamp(0, clip)) / 512) as u8;
                assert_eq!(fast[j], expected, "lane {j}");
            }
        }
    }

    #[test]
    fn sparse_affine_matches_dense_dot_products() {
        const INPUTS: usize = 128;
        let input: Vec<u8> = (0..INPUTS)
            .map(|i| {
                if i % 3 == 0 || i / 4 % 5 == 0 {
                    0
                } else {
                    (i * 13 % 128) as u8
                }
            })
            .collect();
        let weights: Vec<i8> = (0..16 * INPUTS)
            .map(|i| ((i * 29) % 256) as u8 as i8)
            .collect();
        let biases: [i32; 16] = std::array::from_fn(|o| o as i32 * 100 - 700);

        let sparse = sparse_affine_16(&input, &scramble_for_sparse(&weights, INPUTS, 16), &biases);

        for o in 0..16 {
            let dense =
                biases[o] + dot_u8_i8_scalar(&input, &weights[o * INPUTS..(o + 1) * INPUTS]);
            assert_eq!(sparse[o], dense, "output {o}");
        }
    }
}
