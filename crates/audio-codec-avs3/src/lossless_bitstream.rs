mod entropy;
mod frame;
mod lpc;
mod prepost;
mod stereo;

use yinqidao_codec_core::CodecError;

use crate::{LOSSLESS_RICE_MAX_PREFIX, bitreader::BitReader};

pub use entropy::{
    LOSSLESS_ENTROPY_MODE_BITS, LosslessEntropyMode, decode_lossless_entropy_mode_at,
};
pub use frame::{
    LOSSLESS_FRAME_ERROR_CHECK_BITS, LosslessAatfEnvelope,
    decode_lossless_frame_error_check_at, parse_lossless_aatf_envelope,
};
pub use lpc::{
    LOSSLESS_LPC_Q_BITS, LOSSLESS_LPC_Q_ONE, LOSSLESS_UNIFORM_PARCOR_Q20_HALF_STEP,
    LOSSLESS_UNIFORM_PARCOR_Q20_STEP, dequantize_lossless_uniform_parcor_q20,
    dequantize_lossless_uniform_parcor_tail_q20,
};
pub use prepost::{
    LOSSLESS_PREPROCESS_MAX_SAMPLES, lossless_ra_shift, lossless_ra_shift12,
    lossless_residual_shift_plan,
};
pub use stereo::{
    LosslessStereoDecorrelationMode, restore_lossless_anti_phase,
    restore_lossless_stereo_in_place, restore_lossless_stereo_pair,
};

/// Decode one ordinary MSB-first Golomb-Rice codeword at an arbitrary bit position.
///
/// This helper intentionally handles only the base `q <= 63` form. IEEE 1857.2 / GB/T 33475.3
/// carries a parameter-increase escape when the unary prefix would otherwise exceed 63; the
/// `ll_raw_data_block()` syntax layer must resolve that escape before using this primitive.
pub fn decode_lossless_rice_base_codeword_at(
    bytes: &[u8],
    bit_offset: usize,
    parameter: u8,
) -> Result<(u64, usize), CodecError> {
    if parameter > 63 {
        return Err(CodecError::InvalidData(
            "lossless Rice parameter exceeds 64-bit symbol geometry",
        ));
    }
    let mut reader = BitReader::with_bit_position(bytes, bit_offset)?;
    let mut quotient = 0_u64;
    while !reader.read_bit()? {
        quotient = quotient
            .checked_add(1)
            .ok_or(CodecError::InvalidData("lossless Rice quotient overflows"))?;
        if quotient > LOSSLESS_RICE_MAX_PREFIX {
            return Err(CodecError::InvalidData(
                "lossless Rice prefix requires the parameter escape path",
            ));
        }
    }

    let mut remainder = 0_u64;
    for _ in 0..parameter {
        let bit = if reader.read_bit()? { 1_u64 } else { 0_u64 };
        remainder = remainder
            .checked_shl(1)
            .and_then(|value| value.checked_add(bit))
            .ok_or(CodecError::InvalidData(
                "lossless Rice remainder exceeds 64-bit symbol geometry",
            ))?;
    }
    let value = quotient
        .checked_shl(u32::from(parameter))
        .and_then(|base| base.checked_add(remainder))
        .ok_or(CodecError::InvalidData(
            "lossless Rice codeword exceeds 64-bit symbol geometry",
        ))?;
    Ok((value, reader.position_bits()))
}

/// Restore one prediction residual after the lossless pre-processor's magnitude down-shift.
///
/// `flattened_magnitude` carries `abs(residual) >> shift`; `lsb` carries the removed low bits and
/// `negative` is the independently coded sign. Arithmetic is widened so malformed streams fail
/// rather than wrapping the 16/24-bit PCM working domain.
pub fn restore_lossless_flattened_residual(
    flattened_magnitude: u64,
    lsb: u64,
    shift: u8,
    negative: bool,
) -> Result<i32, CodecError> {
    if shift > 31 {
        return Err(CodecError::InvalidData(
            "lossless residual shift exceeds the 32-bit PCM working domain",
        ));
    }
    let lsb_limit = 1_u64 << u32::from(shift);
    if lsb >= lsb_limit {
        return Err(CodecError::InvalidData(
            "lossless residual LSB exceeds its shift width",
        ));
    }
    let magnitude = flattened_magnitude
        .checked_shl(u32::from(shift))
        .and_then(|value| value.checked_add(lsb))
        .ok_or(CodecError::InvalidData(
            "lossless residual reconstruction overflows",
        ))?;
    let magnitude = i64::try_from(magnitude).map_err(|_| {
        CodecError::InvalidData("lossless residual magnitude exceeds signed working range")
    })?;
    let value = if negative { -magnitude } else { magnitude };
    i32::try_from(value)
        .map_err(|_| CodecError::InvalidData("lossless residual exceeds i32 working range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base_rice_codeword_without_repacking() {
        // q=3 => 0001, m=2 remainder=2 => 10. Symbol = (3<<2)|2 = 14.
        let (value, next) =
            decode_lossless_rice_base_codeword_at(&[0b0001_1000], 0, 2).unwrap();
        assert_eq!(value, 14);
        assert_eq!(next, 6);
    }

    #[test]
    fn base_reader_rejects_prefix_that_requires_escape() {
        let bytes = [0_u8; 9];
        assert!(decode_lossless_rice_base_codeword_at(&bytes, 0, 0).is_err());
    }

    #[test]
    fn restores_flattened_residual_with_direct_low_bits() {
        assert_eq!(restore_lossless_flattened_residual(7, 3, 2, false).unwrap(), 31);
        assert_eq!(restore_lossless_flattened_residual(7, 3, 2, true).unwrap(), -31);
        assert!(restore_lossless_flattened_residual(7, 4, 2, false).is_err());
    }
}
