//! Cross-platform SIMD kernels shared by YinQiDao's DSP and pure-Rust codec crates.
//!
//! Release binaries remain portable: x86/x86_64 performs runtime dispatch, AArch64 uses the
//! architectural NEON baseline, and every other target keeps a scalar implementation. Codec crates
//! call these primitives instead of scattering target-specific intrinsics through entropy,
//! transform, synthesis and rendering code.

#![deny(unsafe_op_in_unsafe_fn)]

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimdBackend {
    Scalar,
    Sse2,
    Avx2,
    Avx2Fma,
    Neon,
}

static BEST_BACKEND: OnceLock<SimdBackend> = OnceLock::new();

#[inline]
pub fn best_backend() -> SimdBackend {
    *BEST_BACKEND.get_or_init(detect_backend)
}

fn detect_backend() -> SimdBackend {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::arch::is_x86_feature_detected!("avx2")
            && std::arch::is_x86_feature_detected!("fma")
        {
            return SimdBackend::Avx2Fma;
        }
        if std::arch::is_x86_feature_detected!("avx2") {
            return SimdBackend::Avx2;
        }
        if std::arch::is_x86_feature_detected!("sse2") {
            return SimdBackend::Sse2;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        // Advanced SIMD/NEON is part of the AArch64 architecture baseline. This covers modern
        // Android ARM64, iOS/iPadOS, macOS Apple Silicon, Windows ARM64 and Linux ARM64 without a
        // platform API or runtime feature probe.
        return SimdBackend::Neon;
    }

    #[allow(unreachable_code)]
    SimdBackend::Scalar
}

/// Apply a linear gain and hard PCM safety clamp in one pass.
#[inline]
pub fn gain_clamp_in_place(samples: &mut [f32], gain: f32) {
    match best_backend() {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        SimdBackend::Avx2 | SimdBackend::Avx2Fma => {
            // SAFETY: dispatch verifies AVX2 before entering this target-feature function.
            unsafe { x86::gain_clamp_avx2(samples, gain) }
        }
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        SimdBackend::Sse2 => {
            // SAFETY: dispatch verifies SSE2 before entering this target-feature function.
            unsafe { x86::gain_clamp_sse2(samples, gain) }
        }
        #[cfg(target_arch = "aarch64")]
        SimdBackend::Neon => {
            // SAFETY: Advanced SIMD/NEON is mandatory on AArch64.
            unsafe { neon::gain_clamp_neon(samples, gain) }
        }
        _ => scalar_gain_clamp(samples, gain),
    }
}

/// `dst[i] += src[i] * gain` with architecture-specific vectorization.
///
/// This is the common overlap/add and channel/object reconstruction primitive. The shorter slice
/// controls the processed length and no temporary allocation is performed.
#[inline]
pub fn mix_accumulate(dst: &mut [f32], src: &[f32], gain: f32) {
    let len = dst.len().min(src.len());
    let dst = &mut dst[..len];
    let src = &src[..len];
    match best_backend() {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        SimdBackend::Avx2Fma => {
            // SAFETY: AVX2+FMA were runtime-detected and both slices cover `len` elements.
            unsafe { x86::mix_accumulate_avx2_fma(dst, src, gain) }
        }
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        SimdBackend::Avx2 => {
            // SAFETY: AVX2 was runtime-detected and both slices cover `len` elements.
            unsafe { x86::mix_accumulate_avx2(dst, src, gain) }
        }
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        SimdBackend::Sse2 => {
            // SAFETY: SSE2 was runtime-detected and both slices cover `len` elements.
            unsafe { x86::mix_accumulate_sse2(dst, src, gain) }
        }
        #[cfg(target_arch = "aarch64")]
        SimdBackend::Neon => {
            // SAFETY: NEON is mandatory on AArch64 and slices cover `len` elements.
            unsafe { neon::mix_accumulate_neon(dst, src, gain) }
        }
        _ => scalar_mix_accumulate(dst, src, gain),
    }
}

/// Multiply a signal by a same-length transform/window coefficient vector in place.
///
/// MDCT/IMDCT windows, synthesis filters and overlap windows use this kernel heavily.
#[inline]
pub fn multiply_in_place(samples: &mut [f32], coefficients: &[f32]) {
    let len = samples.len().min(coefficients.len());
    let samples = &mut samples[..len];
    let coefficients = &coefficients[..len];
    match best_backend() {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        SimdBackend::Avx2 | SimdBackend::Avx2Fma => {
            // SAFETY: AVX2 was runtime-detected and slices cover `len` elements.
            unsafe { x86::multiply_avx2(samples, coefficients) }
        }
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        SimdBackend::Sse2 => {
            // SAFETY: SSE2 was runtime-detected and slices cover `len` elements.
            unsafe { x86::multiply_sse2(samples, coefficients) }
        }
        #[cfg(target_arch = "aarch64")]
        SimdBackend::Neon => {
            // SAFETY: NEON is mandatory on AArch64 and slices cover `len` elements.
            unsafe { neon::multiply_neon(samples, coefficients) }
        }
        _ => scalar_multiply(samples, coefficients),
    }
}

/// Dot product used by transform, prediction and filter-bank kernels.
#[inline]
pub fn dot_product(left: &[f32], right: &[f32]) -> f32 {
    let len = left.len().min(right.len());
    let left = &left[..len];
    let right = &right[..len];
    match best_backend() {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        SimdBackend::Avx2Fma => {
            // SAFETY: AVX2+FMA were runtime-detected and slices cover `len` elements.
            unsafe { x86::dot_avx2_fma(left, right) }
        }
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        SimdBackend::Avx2 => {
            // SAFETY: AVX2 was runtime-detected and slices cover `len` elements.
            unsafe { x86::dot_avx2(left, right) }
        }
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        SimdBackend::Sse2 => {
            // SAFETY: SSE2 was runtime-detected and slices cover `len` elements.
            unsafe { x86::dot_sse2(left, right) }
        }
        #[cfg(target_arch = "aarch64")]
        SimdBackend::Neon => {
            // SAFETY: NEON is mandatory on AArch64 and slices cover `len` elements.
            unsafe { neon::dot_neon(left, right) }
        }
        _ => scalar_dot(left, right),
    }
}

#[inline]
fn scalar_gain_clamp(samples: &mut [f32], gain: f32) {
    for sample in samples {
        *sample = (*sample * gain).clamp(-1.0, 1.0);
    }
}

#[inline]
fn scalar_mix_accumulate(dst: &mut [f32], src: &[f32], gain: f32) {
    for (dst, src) in dst.iter_mut().zip(src) {
        *dst += *src * gain;
    }
}

#[inline]
fn scalar_multiply(samples: &mut [f32], coefficients: &[f32]) {
    for (sample, coefficient) in samples.iter_mut().zip(coefficients) {
        *sample *= *coefficient;
    }
}

#[inline]
fn scalar_dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter().zip(right).map(|(left, right)| left * right).sum()
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod x86 {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    #[target_feature(enable = "avx2")]
    pub unsafe fn gain_clamp_avx2(samples: &mut [f32], gain: f32) {
        let mut index = 0;
        let vectors = samples.len() / 8 * 8;
        // SAFETY: caller verified AVX2; unaligned accesses stay inside the slice.
        unsafe {
            let gain_v = _mm256_set1_ps(gain);
            let lo = _mm256_set1_ps(-1.0);
            let hi = _mm256_set1_ps(1.0);
            while index < vectors {
                let value = _mm256_loadu_ps(samples.as_ptr().add(index));
                let value = _mm256_mul_ps(value, gain_v);
                let value = _mm256_min_ps(_mm256_max_ps(value, lo), hi);
                _mm256_storeu_ps(samples.as_mut_ptr().add(index), value);
                index += 8;
            }
        }
        super::scalar_gain_clamp(&mut samples[index..], gain);
    }

    #[target_feature(enable = "sse2")]
    pub unsafe fn gain_clamp_sse2(samples: &mut [f32], gain: f32) {
        let mut index = 0;
        let vectors = samples.len() / 4 * 4;
        // SAFETY: caller verified SSE2; unaligned accesses stay inside the slice.
        unsafe {
            let gain_v = _mm_set1_ps(gain);
            let lo = _mm_set1_ps(-1.0);
            let hi = _mm_set1_ps(1.0);
            while index < vectors {
                let value = _mm_loadu_ps(samples.as_ptr().add(index));
                let value = _mm_mul_ps(value, gain_v);
                let value = _mm_min_ps(_mm_max_ps(value, lo), hi);
                _mm_storeu_ps(samples.as_mut_ptr().add(index), value);
                index += 4;
            }
        }
        super::scalar_gain_clamp(&mut samples[index..], gain);
    }

    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn mix_accumulate_avx2_fma(dst: &mut [f32], src: &[f32], gain: f32) {
        let mut index = 0;
        let vectors = dst.len() / 8 * 8;
        // SAFETY: caller verified AVX2+FMA; all accesses stay inside equal-length slices.
        unsafe {
            let gain_v = _mm256_set1_ps(gain);
            while index < vectors {
                let dst_v = _mm256_loadu_ps(dst.as_ptr().add(index));
                let src_v = _mm256_loadu_ps(src.as_ptr().add(index));
                let mixed = _mm256_fmadd_ps(src_v, gain_v, dst_v);
                _mm256_storeu_ps(dst.as_mut_ptr().add(index), mixed);
                index += 8;
            }
        }
        super::scalar_mix_accumulate(&mut dst[index..], &src[index..], gain);
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn mix_accumulate_avx2(dst: &mut [f32], src: &[f32], gain: f32) {
        let mut index = 0;
        let vectors = dst.len() / 8 * 8;
        // SAFETY: caller verified AVX2; all accesses stay inside equal-length slices.
        unsafe {
            let gain_v = _mm256_set1_ps(gain);
            while index < vectors {
                let dst_v = _mm256_loadu_ps(dst.as_ptr().add(index));
                let src_v = _mm256_loadu_ps(src.as_ptr().add(index));
                let mixed = _mm256_add_ps(dst_v, _mm256_mul_ps(src_v, gain_v));
                _mm256_storeu_ps(dst.as_mut_ptr().add(index), mixed);
                index += 8;
            }
        }
        super::scalar_mix_accumulate(&mut dst[index..], &src[index..], gain);
    }

    #[target_feature(enable = "sse2")]
    pub unsafe fn mix_accumulate_sse2(dst: &mut [f32], src: &[f32], gain: f32) {
        let mut index = 0;
        let vectors = dst.len() / 4 * 4;
        // SAFETY: caller verified SSE2; all accesses stay inside equal-length slices.
        unsafe {
            let gain_v = _mm_set1_ps(gain);
            while index < vectors {
                let dst_v = _mm_loadu_ps(dst.as_ptr().add(index));
                let src_v = _mm_loadu_ps(src.as_ptr().add(index));
                let mixed = _mm_add_ps(dst_v, _mm_mul_ps(src_v, gain_v));
                _mm_storeu_ps(dst.as_mut_ptr().add(index), mixed);
                index += 4;
            }
        }
        super::scalar_mix_accumulate(&mut dst[index..], &src[index..], gain);
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn multiply_avx2(samples: &mut [f32], coefficients: &[f32]) {
        let mut index = 0;
        let vectors = samples.len() / 8 * 8;
        // SAFETY: caller verified AVX2 and all accesses remain in range.
        unsafe {
            while index < vectors {
                let sample_v = _mm256_loadu_ps(samples.as_ptr().add(index));
                let coefficient_v = _mm256_loadu_ps(coefficients.as_ptr().add(index));
                _mm256_storeu_ps(
                    samples.as_mut_ptr().add(index),
                    _mm256_mul_ps(sample_v, coefficient_v),
                );
                index += 8;
            }
        }
        super::scalar_multiply(&mut samples[index..], &coefficients[index..]);
    }

    #[target_feature(enable = "sse2")]
    pub unsafe fn multiply_sse2(samples: &mut [f32], coefficients: &[f32]) {
        let mut index = 0;
        let vectors = samples.len() / 4 * 4;
        // SAFETY: caller verified SSE2 and all accesses remain in range.
        unsafe {
            while index < vectors {
                let sample_v = _mm_loadu_ps(samples.as_ptr().add(index));
                let coefficient_v = _mm_loadu_ps(coefficients.as_ptr().add(index));
                _mm_storeu_ps(
                    samples.as_mut_ptr().add(index),
                    _mm_mul_ps(sample_v, coefficient_v),
                );
                index += 4;
            }
        }
        super::scalar_multiply(&mut samples[index..], &coefficients[index..]);
    }

    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn dot_avx2_fma(left: &[f32], right: &[f32]) -> f32 {
        let mut index = 0;
        let vectors = left.len() / 8 * 8;
        let mut lanes = [0.0_f32; 8];
        // SAFETY: caller verified AVX2+FMA and all accesses stay inside the slices.
        unsafe {
            let mut accumulator = _mm256_setzero_ps();
            while index < vectors {
                let left_v = _mm256_loadu_ps(left.as_ptr().add(index));
                let right_v = _mm256_loadu_ps(right.as_ptr().add(index));
                accumulator = _mm256_fmadd_ps(left_v, right_v, accumulator);
                index += 8;
            }
            _mm256_storeu_ps(lanes.as_mut_ptr(), accumulator);
        }
        lanes.into_iter().sum::<f32>() + super::scalar_dot(&left[index..], &right[index..])
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn dot_avx2(left: &[f32], right: &[f32]) -> f32 {
        let mut index = 0;
        let vectors = left.len() / 8 * 8;
        let mut lanes = [0.0_f32; 8];
        // SAFETY: caller verified AVX2 and all accesses stay inside the slices.
        unsafe {
            let mut accumulator = _mm256_setzero_ps();
            while index < vectors {
                let left_v = _mm256_loadu_ps(left.as_ptr().add(index));
                let right_v = _mm256_loadu_ps(right.as_ptr().add(index));
                accumulator = _mm256_add_ps(accumulator, _mm256_mul_ps(left_v, right_v));
                index += 8;
            }
            _mm256_storeu_ps(lanes.as_mut_ptr(), accumulator);
        }
        lanes.into_iter().sum::<f32>() + super::scalar_dot(&left[index..], &right[index..])
    }

    #[target_feature(enable = "sse2")]
    pub unsafe fn dot_sse2(left: &[f32], right: &[f32]) -> f32 {
        let mut index = 0;
        let vectors = left.len() / 4 * 4;
        let mut lanes = [0.0_f32; 4];
        // SAFETY: caller verified SSE2 and all accesses stay inside the slices.
        unsafe {
            let mut accumulator = _mm_setzero_ps();
            while index < vectors {
                let left_v = _mm_loadu_ps(left.as_ptr().add(index));
                let right_v = _mm_loadu_ps(right.as_ptr().add(index));
                accumulator = _mm_add_ps(accumulator, _mm_mul_ps(left_v, right_v));
                index += 4;
            }
            _mm_storeu_ps(lanes.as_mut_ptr(), accumulator);
        }
        lanes.into_iter().sum::<f32>() + super::scalar_dot(&left[index..], &right[index..])
    }
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use std::arch::aarch64::*;

    pub unsafe fn gain_clamp_neon(samples: &mut [f32], gain: f32) {
        let mut index = 0;
        let vectors = samples.len() / 4 * 4;
        // SAFETY: AArch64 guarantees Advanced SIMD and accesses stay inside each 4-lane block.
        unsafe {
            let gain_v = vdupq_n_f32(gain);
            let lo = vdupq_n_f32(-1.0);
            let hi = vdupq_n_f32(1.0);
            while index < vectors {
                let value = vld1q_f32(samples.as_ptr().add(index));
                let value = vmulq_f32(value, gain_v);
                let value = vminq_f32(vmaxq_f32(value, lo), hi);
                vst1q_f32(samples.as_mut_ptr().add(index), value);
                index += 4;
            }
        }
        super::scalar_gain_clamp(&mut samples[index..], gain);
    }

    pub unsafe fn mix_accumulate_neon(dst: &mut [f32], src: &[f32], gain: f32) {
        let mut index = 0;
        let vectors = dst.len() / 4 * 4;
        // SAFETY: AArch64 guarantees Advanced SIMD and both slices cover every vector access.
        unsafe {
            let gain_v = vdupq_n_f32(gain);
            while index < vectors {
                let dst_v = vld1q_f32(dst.as_ptr().add(index));
                let src_v = vld1q_f32(src.as_ptr().add(index));
                let mixed = vmlaq_f32(dst_v, src_v, gain_v);
                vst1q_f32(dst.as_mut_ptr().add(index), mixed);
                index += 4;
            }
        }
        super::scalar_mix_accumulate(&mut dst[index..], &src[index..], gain);
    }

    pub unsafe fn multiply_neon(samples: &mut [f32], coefficients: &[f32]) {
        let mut index = 0;
        let vectors = samples.len() / 4 * 4;
        // SAFETY: AArch64 guarantees Advanced SIMD and accesses remain inside the slices.
        unsafe {
            while index < vectors {
                let sample_v = vld1q_f32(samples.as_ptr().add(index));
                let coefficient_v = vld1q_f32(coefficients.as_ptr().add(index));
                vst1q_f32(samples.as_mut_ptr().add(index), vmulq_f32(sample_v, coefficient_v));
                index += 4;
            }
        }
        super::scalar_multiply(&mut samples[index..], &coefficients[index..]);
    }

    pub unsafe fn dot_neon(left: &[f32], right: &[f32]) -> f32 {
        let mut index = 0;
        let vectors = left.len() / 4 * 4;
        let mut lanes = [0.0_f32; 4];
        // SAFETY: AArch64 guarantees Advanced SIMD and accesses stay inside the slices.
        unsafe {
            let mut accumulator = vdupq_n_f32(0.0);
            while index < vectors {
                let left_v = vld1q_f32(left.as_ptr().add(index));
                let right_v = vld1q_f32(right.as_ptr().add(index));
                accumulator = vmlaq_f32(accumulator, left_v, right_v);
                index += 4;
            }
            vst1q_f32(lanes.as_mut_ptr(), accumulator);
        }
        lanes.into_iter().sum::<f32>() + super::scalar_dot(&left[index..], &right[index..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_clamp_matches_scalar_reference() {
        let mut actual = (-257..257)
            .map(|index| index as f32 / 96.0)
            .collect::<Vec<_>>();
        let mut expected = actual.clone();
        scalar_gain_clamp(&mut expected, 0.73);
        gain_clamp_in_place(&mut actual, 0.73);
        assert_eq!(actual, expected);
    }

    #[test]
    fn mix_accumulate_matches_scalar_reference() {
        let src = (0..513)
            .map(|index| (index as f32 * 0.03125).sin())
            .collect::<Vec<_>>();
        let mut actual = vec![0.25; src.len()];
        let mut expected = actual.clone();
        scalar_mix_accumulate(&mut expected, &src, -0.37);
        mix_accumulate(&mut actual, &src, -0.37);
        for (actual, expected) in actual.iter().zip(expected) {
            assert!((actual - expected).abs() < 2.0e-6);
        }
    }

    #[test]
    fn multiply_matches_scalar_reference() {
        let coefficients = (0..517)
            .map(|index| 0.25 + index as f32 / 1000.0)
            .collect::<Vec<_>>();
        let mut actual = (0..517)
            .map(|index| (index as f32 * 0.017).cos())
            .collect::<Vec<_>>();
        let mut expected = actual.clone();
        scalar_multiply(&mut expected, &coefficients);
        multiply_in_place(&mut actual, &coefficients);
        for (actual, expected) in actual.iter().zip(expected) {
            assert!((actual - expected).abs() < 1.0e-6);
        }
    }

    #[test]
    fn dot_product_matches_scalar_reference() {
        let left = (0..521)
            .map(|index| (index as f32 * 0.013).sin())
            .collect::<Vec<_>>();
        let right = (0..521)
            .map(|index| (index as f32 * 0.021).cos())
            .collect::<Vec<_>>();
        let expected = scalar_dot(&left, &right);
        let actual = dot_product(&left, &right);
        assert!((actual - expected).abs() < 2.0e-4);
    }
}
