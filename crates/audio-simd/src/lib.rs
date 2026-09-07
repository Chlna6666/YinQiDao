//! Cross-platform SIMD kernels shared by YinQiDao's DSP and pure-Rust codec crates.
//!
//! Dispatch is performed at runtime on x86/x86_64 so release binaries remain portable across
//! different Intel/AMD generations. AArch64 uses NEON, which is part of the architecture baseline
//! on Windows ARM64, Linux ARM64, Android ARM64 and iOS ARM64. Unknown architectures retain a
//! scalar implementation instead of failing to compile.

#![deny(unsafe_op_in_unsafe_fn)]

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimdBackend {
    Scalar,
    Sse2,
    Avx2,
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
        if std::arch::is_x86_feature_detected!("avx2") {
            return SimdBackend::Avx2;
        }
        if std::arch::is_x86_feature_detected!("sse2") {
            return SimdBackend::Sse2;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        return SimdBackend::Neon;
    }

    #[allow(unreachable_code)]
    SimdBackend::Scalar
}

/// Apply a linear gain and hard PCM safety clamp in one pass.
///
/// This is intentionally a generic primitive: playback volume, decoder synthesis and future codec
/// post-filters can share the same runtime dispatch without duplicating architecture-specific code.
#[inline]
pub fn gain_clamp_in_place(samples: &mut [f32], gain: f32) {
    match best_backend() {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        SimdBackend::Avx2 => {
            // SAFETY: the dispatch above verifies AVX2 before entering the target-feature function.
            unsafe { x86::gain_clamp_avx2(samples, gain) }
        }
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        SimdBackend::Sse2 => {
            // SAFETY: the dispatch above verifies SSE2 before entering the target-feature function.
            unsafe { x86::gain_clamp_sse2(samples, gain) }
        }
        #[cfg(target_arch = "aarch64")]
        SimdBackend::Neon => {
            // SAFETY: Advanced SIMD/NEON is part of the AArch64 architectural baseline.
            unsafe { neon::gain_clamp_neon(samples, gain) }
        }
        _ => scalar_gain_clamp(samples, gain),
    }
}

/// `dst[i] += src[i] * gain` with architecture-specific vectorization.
///
/// The shorter slice controls the processed length. This is useful for overlap/add, channel
/// reconstruction and object/HOA rendering where codec kernels repeatedly accumulate vectors.
#[inline]
pub fn mix_accumulate(dst: &mut [f32], src: &[f32], gain: f32) {
    let len = dst.len().min(src.len());
    let dst = &mut dst[..len];
    let src = &src[..len];
    match best_backend() {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        SimdBackend::Avx2 => {
            // SAFETY: AVX2 was runtime-detected and both slices are valid for `len` elements.
            unsafe { x86::mix_accumulate_avx2(dst, src, gain) }
        }
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        SimdBackend::Sse2 => {
            // SAFETY: SSE2 was runtime-detected and both slices are valid for `len` elements.
            unsafe { x86::mix_accumulate_sse2(dst, src, gain) }
        }
        #[cfg(target_arch = "aarch64")]
        SimdBackend::Neon => {
            // SAFETY: NEON is mandatory on AArch64 and slices are valid for `len` elements.
            unsafe { neon::mix_accumulate_neon(dst, src, gain) }
        }
        _ => scalar_mix_accumulate(dst, src, gain),
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
        // SAFETY: the caller verified AVX2. Every load/store remains inside the slice and uses the
        // unaligned variants, so no additional alignment contract is required.
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
        // SAFETY: the caller verified SSE2 and all unaligned accesses remain inside the slice.
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

    #[target_feature(enable = "avx2")]
    pub unsafe fn mix_accumulate_avx2(dst: &mut [f32], src: &[f32], gain: f32) {
        let mut index = 0;
        let vectors = dst.len() / 8 * 8;
        // SAFETY: the caller verified AVX2; src/dst have equal lengths and accesses stay in range.
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
        // SAFETY: the caller verified SSE2; src/dst have equal lengths and accesses stay in range.
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
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use std::arch::aarch64::*;

    pub unsafe fn gain_clamp_neon(samples: &mut [f32], gain: f32) {
        let mut index = 0;
        let vectors = samples.len() / 4 * 4;
        // SAFETY: AArch64 guarantees Advanced SIMD. Accesses are valid for each 4-lane block.
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
                let mixed = vaddq_f32(dst_v, vmulq_f32(src_v, gain_v));
                vst1q_f32(dst.as_mut_ptr().add(index), mixed);
                index += 4;
            }
        }
        super::scalar_mix_accumulate(&mut dst[index..], &src[index..], gain);
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
            assert!((actual - expected).abs() < 1.0e-6);
        }
    }
}
