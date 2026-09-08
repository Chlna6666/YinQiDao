use yinqidao_codec_core::CodecError;

/// Minimum standardized lossless LPC order exercised by the AVS2 conformance suite.
pub const LOSSLESS_LPC_ORDER_MIN: u8 = 1;
/// Maximum standardized lossless LPC order; conformance traversal explicitly reaches order 127.
pub const LOSSLESS_LPC_ORDER_MAX: u8 = 127;
/// Fractional precision used by the lossless predictor's fixed-point PARCOR/LPC domain.
pub const LOSSLESS_LPC_Q_BITS: u8 = 20;
/// Fixed-point representation of unity in the lossless Q20 predictor domain.
pub const LOSSLESS_LPC_Q_ONE: i32 = 1_i32 << LOSSLESS_LPC_Q_BITS;
/// Q20 step corresponding to one unit of the 7-bit uniform PARCOR index (`2^20 / 64`).
pub const LOSSLESS_UNIFORM_PARCOR_Q20_STEP: i32 = 1_i32 << 14;
/// Half-step bias used to reconstruct the center of a uniform PARCOR quantization bin.
pub const LOSSLESS_UNIFORM_PARCOR_Q20_HALF_STEP: i32 = 1_i32 << 13;

/// Validate the semantic LPC order without making any assumption about its eventual wire width.
#[inline]
pub fn validate_lossless_lpc_order(order: u8) -> Result<usize, CodecError> {
    if !(LOSSLESS_LPC_ORDER_MIN..=LOSSLESS_LPC_ORDER_MAX).contains(&order) {
        return Err(CodecError::InvalidData(
            "lossless LPC order is outside the standardized 1..=127 range",
        ));
    }
    Ok(usize::from(order))
}

/// Reconstruct a third-or-later lossless PARCOR coefficient in the normative Q20 domain.
///
/// For quantized index `b` in `[-64, 63]`, the standardized fixed-point reconstruction is
/// `b * 2^14 + 2^13`, i.e. the center `(b + 0.5) / 64` of the quantization interval represented
/// with twenty fractional bits. The first two PARCOR coefficients use the separate nonlinear
/// `Gamma(b)` mapping and must not be passed to this helper merely by coefficient position.
#[inline]
pub fn dequantize_lossless_uniform_parcor_q20(quantized: i8) -> Result<i32, CodecError> {
    if !(-64..=63).contains(&quantized) {
        return Err(CodecError::InvalidData(
            "lossless uniform PARCOR index is outside -64..=63",
        ));
    }

    Ok(i32::from(quantized) * LOSSLESS_UNIFORM_PARCOR_Q20_STEP
        + LOSSLESS_UNIFORM_PARCOR_Q20_HALF_STEP)
}

/// Reconstruct the uniformly quantized PARCOR tail (`k >= 3`) into a caller-provided Q20 buffer.
///
/// No allocation or floating-point arithmetic is performed. `output` may be longer than `input`;
/// only the first `input.len()` entries are initialized.
pub fn dequantize_lossless_uniform_parcor_tail_q20(
    input: &[i8],
    output: &mut [i32],
) -> Result<usize, CodecError> {
    if output.len() < input.len() {
        return Err(CodecError::InvalidData(
            "lossless Q20 PARCOR output is shorter than the uniform coefficient tail",
        ));
    }

    for (dst, &quantized) in output.iter_mut().zip(input) {
        *dst = dequantize_lossless_uniform_parcor_q20(quantized)?;
    }
    Ok(input.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_full_standardized_lpc_order_range() {
        assert_eq!(validate_lossless_lpc_order(1).unwrap(), 1);
        assert_eq!(validate_lossless_lpc_order(127).unwrap(), 127);
        assert!(validate_lossless_lpc_order(0).is_err());
        assert!(validate_lossless_lpc_order(128).is_err());
        assert!(validate_lossless_lpc_order(u8::MAX).is_err());
    }

    #[test]
    fn q20_constants_match_uniform_parcor_geometry() {
        assert_eq!(LOSSLESS_LPC_Q_BITS, 20);
        assert_eq!(LOSSLESS_LPC_Q_ONE, 1_048_576);
        assert_eq!(LOSSLESS_UNIFORM_PARCOR_Q20_STEP, 16_384);
        assert_eq!(LOSSLESS_UNIFORM_PARCOR_Q20_HALF_STEP, 8_192);
    }

    #[test]
    fn uniform_parcor_reconstructs_quantization_bin_centers() {
        assert_eq!(
            dequantize_lossless_uniform_parcor_q20(-64).unwrap(),
            -1_040_384
        );
        assert_eq!(
            dequantize_lossless_uniform_parcor_q20(-1).unwrap(),
            -8_192
        );
        assert_eq!(dequantize_lossless_uniform_parcor_q20(0).unwrap(), 8_192);
        assert_eq!(
            dequantize_lossless_uniform_parcor_q20(63).unwrap(),
            1_040_384
        );
    }

    #[test]
    fn uniform_parcor_midpoints_are_sign_symmetric() {
        for quantized in -64_i8..=63 {
            let mirror = -i16::from(quantized) - 1;
            let mirror = i8::try_from(mirror).unwrap();
            assert_eq!(
                dequantize_lossless_uniform_parcor_q20(quantized).unwrap(),
                -dequantize_lossless_uniform_parcor_q20(mirror).unwrap()
            );
        }
    }

    #[test]
    fn tail_conversion_is_allocation_free_and_checks_geometry() {
        let input = [-64_i8, -1, 0, 63];
        let mut output = [0_i32; 4];
        assert_eq!(
            dequantize_lossless_uniform_parcor_tail_q20(&input, &mut output).unwrap(),
            4
        );
        assert_eq!(output, [-1_040_384, -8_192, 8_192, 1_040_384]);

        let mut short = [0_i32; 3];
        assert!(dequantize_lossless_uniform_parcor_tail_q20(&input, &mut short).is_err());
    }

    #[test]
    fn rejects_reserved_i8_values_outside_seven_bit_index_range() {
        assert!(dequantize_lossless_uniform_parcor_q20(-65).is_err());
        assert!(dequantize_lossless_uniform_parcor_q20(64).is_err());
        assert!(dequantize_lossless_uniform_parcor_q20(i8::MIN).is_err());
        assert!(dequantize_lossless_uniform_parcor_q20(i8::MAX).is_err());
    }
}
