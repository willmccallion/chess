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
}
