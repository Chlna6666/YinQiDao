/// SIMD-accelerated overlap/add primitive used by transform codecs.
pub fn overlap_add(dst: &mut [f32], src: &[f32], gain: f32) {
    yinqidao_audio_simd::mix_accumulate(dst, src, gain);
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
}
