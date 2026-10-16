//! Dot products of an f32 query with f16 rows: the whole cost of a search.
//!
//! The rows stay f16 in the mapped file and are widened eight at a time as they are read. On a
//! processor with AVX2, FMA and F16C (every x86-64 made since about 2013, and this machine) the
//! widening is one instruction (`vcvtph2ps`) and four independent accumulators keep the FMA units
//! busy; elsewhere a portable loop does the same work in blocks the compiler can vectorise.

use std::sync::LazyLock;

use half::f16;

/// Whether the processor has what the fast kernel uses, asked once.
static FAST: LazyLock<bool> = LazyLock::new(|| {
    #[cfg(target_arch = "x86_64")]
    {
        is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") && is_x86_feature_detected!("f16c")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
});

/// The dot product of `query` with the f16 bits in `row`, which has the same length.
#[inline]
pub fn dot(query: &[f32], row: &[u16]) -> f32 {
    debug_assert_eq!(query.len(), row.len());
    #[cfg(target_arch = "x86_64")]
    if *FAST {
        // SAFETY: the processor was checked for every feature `dot_avx2` is compiled for.
        return unsafe { dot_avx2(query, row) };
    }
    dot_portable(query, row)
}

/// Which kernel searches use, for benchmarks and logs.
pub fn kernel_name() -> &'static str {
    if *FAST { "avx2+fma+f16c" } else { "portable" }
}

/// Widens eight halves at a time into a block, then multiplies it with eight accumulators the
/// compiler keeps in vector registers.
pub fn dot_portable(query: &[f32], row: &[u16]) -> f32 {
    const BLOCK: usize = 64;
    let mut acc = [0f32; 8];
    let mut wide = [0f32; BLOCK];
    let mut q_blocks = query.chunks_exact(BLOCK);
    let mut r_blocks = row.chunks_exact(BLOCK);
    for (q, r) in (&mut q_blocks).zip(&mut r_blocks) {
        let halves: &[f16] = bytemuck::cast_slice(r);
        half::slice::HalfFloatSliceExt::convert_to_f32_slice(halves, &mut wide);
        for (q8, w8) in q.chunks_exact(8).zip(wide.chunks_exact(8)) {
            for lane in 0..8 {
                acc[lane] += q8[lane] * w8[lane];
            }
        }
    }
    let mut sum: f32 = acc.iter().sum();
    for (q, &r) in q_blocks.remainder().iter().zip(r_blocks.remainder()) {
        sum += q * f16::from_bits(r).to_f32();
    }
    sum
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma,f16c")]
unsafe fn dot_avx2(query: &[f32], row: &[u16]) -> f32 {
    use std::arch::x86_64::*;
    let n = query.len().min(row.len());
    let q = query.as_ptr();
    let r = row.as_ptr();
    let mut acc = [_mm256_setzero_ps(); 4];
    let mut i = 0;
    // SAFETY: every load reads eight elements at `i..i + 8`, and each loop only runs while
    // that range lies inside both slices (`n` is the shorter length). Unaligned loads are used.
    unsafe {
        while i + 32 <= n {
            for (lane, sum) in acc.iter_mut().enumerate() {
                let at = i + lane * 8;
                let halves = _mm_loadu_si128(r.add(at) as *const __m128i);
                *sum = _mm256_fmadd_ps(_mm256_loadu_ps(q.add(at)), _mm256_cvtph_ps(halves), *sum);
            }
            i += 32;
        }
        while i + 8 <= n {
            let halves = _mm_loadu_si128(r.add(i) as *const __m128i);
            acc[0] = _mm256_fmadd_ps(_mm256_loadu_ps(q.add(i)), _mm256_cvtph_ps(halves), acc[0]);
            i += 8;
        }
    }
    let total = _mm256_add_ps(_mm256_add_ps(acc[0], acc[1]), _mm256_add_ps(acc[2], acc[3]));
    let mut lanes = [0f32; 8];
    // SAFETY: `lanes` holds exactly eight f32s; the store is unaligned.
    unsafe { _mm256_storeu_ps(lanes.as_mut_ptr(), total) };
    let mut sum: f32 = lanes.iter().sum();
    for j in i..n {
        sum += query[j] * f16::from_bits(row[j]).to_f32();
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    fn halves(values: &[f32]) -> Vec<u16> {
        values.iter().map(|&v| f16::from_f32(v).to_bits()).collect()
    }

    #[test]
    fn kernels_agree_with_a_plain_loop_at_every_length() {
        for n in [0usize, 1, 7, 8, 9, 31, 32, 33, 64, 65, 100, 768, 1152] {
            let query: Vec<f32> = (0..n).map(|i| ((i * 37 % 101) as f32 - 50.0) / 97.0).collect();
            let row: Vec<f32> = (0..n).map(|i| ((i * 53 % 89) as f32 - 44.0) / 61.0).collect();
            let bits = halves(&row);
            let plain: f32 = query.iter().zip(&bits).map(|(q, &h)| q * f16::from_bits(h).to_f32()).sum();
            let fast = dot(&query, &bits);
            let portable = dot_portable(&query, &bits);
            assert!((fast - plain).abs() <= 1e-3 * (1.0 + plain.abs()), "n={n}: {fast} vs {plain}");
            assert!((portable - plain).abs() <= 1e-3 * (1.0 + plain.abs()), "n={n}: {portable} vs {plain}");
        }
    }
}
