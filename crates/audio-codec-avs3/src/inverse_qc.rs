use yinqidao_codec_core::CodecError;

/// Dequantize the basic-profile scale factor from GY/T 363-2023 section 7.3.3.9.
#[inline]
pub fn basic_feature_scale(is_feat_amplified: bool, scale_q_idx: u8) -> f32 {
    if is_feat_amplified {
        10.0_f32.powf(f32::from(scale_q_idx) / 86.0)
    } else {
        f32::from(scale_q_idx) / 127.0
    }
}

/// Dequantize the low-complexity 8-bit MDCT scale index.
///
/// This is formula (4) in section 7.3.3.9: the quantizer maps the logarithmic scale interval into
/// 256 indices with 31.875 steps per decade and index 255 representing unity.
#[inline]
pub fn low_complexity_feature_scale(scale_q_idx_lc: u8) -> f32 {
    10.0_f32.powf((f32::from(scale_q_idx_lc) - 255.0) / 31.875)
}

/// Dequantize the 3-bit noise-filling index from formula (3).
#[inline]
pub fn noise_filling_parameter(index: u8) -> Result<f32, CodecError> {
    if index > 7 {
        return Err(CodecError::InvalidData(
            "noise-filling quantization index exceeds three-bit domain",
        ));
    }
    Ok(f32::from(index) / 23.34)
}

/// Apply the decoder-side scale adjustment in place.
///
/// Valid streams may code a basic non-amplified scale index of zero. The reference behavior treats
/// a resulting zero scale as unity before division, avoiding NaN/Inf propagation on all-zero latent
/// blocks. The same safety rule is retained here explicitly.
pub fn inverse_scale_in_place(values: &mut [f32], feature_scale: f32) -> Result<(), CodecError> {
    if !feature_scale.is_finite() || feature_scale < 0.0 {
        return Err(CodecError::InvalidData(
            "non-finite or negative AVS3 feature scale",
        ));
    }
    let divisor = if feature_scale == 0.0 {
        1.0
    } else {
        feature_scale
    };
    let gain = divisor.recip();
    for value in values {
        *value *= gain;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_scale_matches_normative_linear_and_log_branches() {
        assert_eq!(basic_feature_scale(false, 127), 1.0);
        assert_eq!(basic_feature_scale(false, 0), 0.0);
        assert!((basic_feature_scale(true, 86) - 10.0).abs() < 1.0e-5);
    }

    #[test]
    fn low_complexity_scale_has_unity_at_index_255() {
        assert!((low_complexity_feature_scale(255) - 1.0).abs() < f32::EPSILON);
        assert!(low_complexity_feature_scale(0) > 0.0);
        assert!(low_complexity_feature_scale(0) < 1.0e-7);
    }

    #[test]
    fn noise_parameter_stays_inside_three_bit_domain() {
        assert_eq!(noise_filling_parameter(0).unwrap(), 0.0);
        assert!((noise_filling_parameter(7).unwrap() - 7.0 / 23.34).abs() < 1.0e-7);
        assert!(noise_filling_parameter(8).is_err());
    }

    #[test]
    fn zero_scale_uses_unity_safety_rule() {
        let mut values = [0.0, 1.0, -2.0];
        inverse_scale_in_place(&mut values, 0.0).unwrap();
        assert_eq!(values, [0.0, 1.0, -2.0]);
    }
}
