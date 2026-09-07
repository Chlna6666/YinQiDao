/// SIMD-accelerated overlap/add primitive used by transform codecs.
pub fn overlap_add(dst: &mut [f32], src: &[f32], gain: f32) {
    yinqidao_audio_simd::mix_accumulate(dst, src, gain);
}

/// SIMD-accelerated transform/window multiplication.
pub fn apply_window_in_place(samples: &mut [f32], window: &[f32]) {
    yinqidao_audio_simd::multiply_in_place(samples, window);
}

/// SIMD-accelerated dot product for transform, prediction and synthesis-filter kernels.
pub fn spectral_dot(left: &[f32], right: &[f32]) -> f32 {
    yinqidao_audio_simd::dot_product(left, right)
}

/// SIMD-accelerated PCM scaling with final hard safety clamp.
pub fn scale_pcm_in_place(samples: &mut [f32], gain: f32) {
    yinqidao_audio_simd::gain_clamp_in_place(samples, gain);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_add_accumulates_without_allocating() {
        let mut dst = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let src = vec![2.0, 2.0, 2.0, 2.0, 2.0];
        overlap_add(&mut dst, &src, 0.5);
        assert_eq!(dst, vec![2.0, 3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn window_and_dot_match_reference() {
        let window = [0.25_f32, 0.5, 0.75, 1.0, 0.5];
        let mut samples = [1.0_f32, -2.0, 3.0, -4.0, 5.0];
        apply_window_in_place(&mut samples, &window);
        assert_eq!(samples, [0.25, -1.0, 2.25, -4.0, 2.5]);

        let dot = spectral_dot(&samples, &window);
        let expected = samples
            .iter()
            .zip(window)
            .map(|(sample, coefficient)| sample * coefficient)
            .sum::<f32>();
        assert!((dot - expected).abs() < 1.0e-5);
    }
}
