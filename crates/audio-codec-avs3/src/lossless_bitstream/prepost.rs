use yinqidao_codec_core::CodecError;

/// The IEEE 1857.2 lossless pre/post processor only reshapes the first 16 prediction residuals.
pub const LOSSLESS_PREPROCESS_MAX_SAMPLES: usize = 16;

// Fixed interoperability tables used by the IEEE 1857.2 / GB/T 33475.3-2018 residual
// pre/post-processor. `RA_SHIFT12` is indexed by q+64 for q in -64..=63; `RA_SHIFT` is indexed by
// abs(q) in 0..=64. Keeping the integer tables in Rust avoids libm and guarantees device-identical
// shift decisions.
const RA_SHIFT: [u16; 65] = [
    0, 1, 6, 13, 23, 36, 52, 71, 93, 118, 146, 177, 211, 249, 290, 334, 381, 432, 487,
    545, 607, 673, 743, 817, 896, 978, 1066, 1158, 1255, 1358, 1466, 1580, 1700, 1826,
    1960, 2100, 2248, 2404, 2569, 2743, 2927, 3122, 3329, 3548, 3781, 4030, 4296, 4580,
    4885, 5214, 5570, 5956, 6378, 6841, 7354, 7927, 8573, 9313, 10176, 11205, 12476,
    14128, 16477, 20526, 23147,
];

const RA_SHIFT12: [u16; 128] = [
    58348, 48794, 43108, 39207, 36249, 33866, 31870, 30151, 28643, 27298, 26083, 24977,
    23959, 23018, 22141, 21321, 20551, 19824, 19136, 18483, 17862, 17269, 16702, 16159,
    15638, 15136, 14654, 14189, 13740, 13305, 12885, 12479, 12084, 11702, 11330, 10969,
    10618, 10277, 9944, 9620, 9305, 8997, 8697, 8404, 8118, 7839, 7566, 7300, 7039, 6785,
    6536, 6293, 6055, 5822, 5594, 5372, 5154, 4941, 4733, 4529, 4330, 4135, 3944, 3758,
    3577, 3399, 3226, 3056, 2891, 2730, 2573, 2420, 2271, 2127, 1986, 1849, 1717, 1589,
    1465, 1345, 1229, 1118, 1012, 909, 812, 719, 631, 548, 469, 397, 329, 267, 211, 161,
    117, 79, 49, 25, 9, 1, 1, 10, 29, 58, 98, 149, 213, 291, 384, 493, 621, 769, 939,
    1135, 1360, 1618, 1915, 2258, 2655, 3119, 3667, 4320, 5117, 6113, 7409, 9209, 12043,
    18365,
];

#[inline]
fn validate_quantized_parcor(value: i8) -> Result<(), CodecError> {
    if (-64..=63).contains(&value) {
        Ok(())
    } else {
        Err(CodecError::InvalidData(
            "lossless quantized PARCOR coefficient is outside -64..=63",
        ))
    }
}

/// Return one fixed `RA_shift12` entry for a quantized PARCOR coefficient.
pub fn lossless_ra_shift12(value: i8) -> Result<u16, CodecError> {
    validate_quantized_parcor(value)?;
    let index = i16::from(value) + 64;
    Ok(RA_SHIFT12[usize::try_from(index).map_err(|_| {
        CodecError::InvalidData("lossless RA_shift12 index conversion failed")
    })?])
}

/// Return one fixed `RA_shift` entry for the magnitude of a quantized PARCOR coefficient.
pub fn lossless_ra_shift(value: i8) -> Result<u16, CodecError> {
    validate_quantized_parcor(value)?;
    let magnitude = i16::from(value).unsigned_abs();
    Ok(RA_SHIFT[usize::from(magnitude)])
}

/// Compute the exact residual down/up-shift plan from quantized PARCOR coefficients.
///
/// The first two coefficients use the special `RA_shift12` table. Starting at coefficient three,
/// the first-two contribution is retained and `RA_shift[abs(q)]` is accumulated. Only the first
/// `min(order, 16)` prediction residuals are reshaped; later residuals have shift zero.
///
/// Returns the number of initialized entries in `output` and performs no allocation.
pub fn lossless_residual_shift_plan(
    quantized_parcor: &[i8],
    output: &mut [u8],
) -> Result<usize, CodecError> {
    let count = quantized_parcor.len().min(LOSSLESS_PREPROCESS_MAX_SAMPLES);
    if output.len() < count {
        return Err(CodecError::InvalidData(
            "lossless residual shift output is shorter than the active PARCOR prefix",
        ));
    }
    if count == 0 {
        return Ok(0);
    }
    for &value in &quantized_parcor[..count] {
        validate_quantized_parcor(value)?;
    }

    let mut accumulated = u32::from(lossless_ra_shift12(quantized_parcor[0])?);
    output[0] = u8::try_from((4096_u32 + accumulated) >> 13).map_err(|_| {
        CodecError::InvalidData("lossless residual shift exceeds u8")
    })?;

    if count >= 2 {
        accumulated = accumulated
            .checked_add(u32::from(lossless_ra_shift12(quantized_parcor[1])?))
            .ok_or(CodecError::InvalidData(
                "lossless residual shift accumulator overflows",
            ))?;
        output[1] = u8::try_from((4096_u32 + accumulated) >> 13).map_err(|_| {
            CodecError::InvalidData("lossless residual shift exceeds u8")
        })?;
    }

    for index in 2..count {
        accumulated = accumulated
            .checked_add(u32::from(lossless_ra_shift(quantized_parcor[index])?))
            .ok_or(CodecError::InvalidData(
                "lossless residual shift accumulator overflows",
            ))?;
        output[index] = u8::try_from((4096_u32 + accumulated) >> 13).map_err(|_| {
            CodecError::InvalidData("lossless residual shift exceeds u8")
        })?;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_tables_match_interoperability_anchor_values() {
        assert_eq!(lossless_ra_shift12(-64).unwrap(), 58_348);
        assert_eq!(lossless_ra_shift12(0).unwrap(), 3_577);
        assert_eq!(lossless_ra_shift12(35).unwrap(), 1);
        assert_eq!(lossless_ra_shift12(63).unwrap(), 18_365);
        assert_eq!(lossless_ra_shift(0).unwrap(), 0);
        assert_eq!(lossless_ra_shift(32).unwrap(), 1_700);
        assert_eq!(lossless_ra_shift(-64).unwrap(), 23_147);
    }

    #[test]
    fn shift_plan_uses_special_first_two_then_magnitude_table() {
        let q = [0_i8, 0, 32, -32];
        let mut shifts = [0_u8; LOSSLESS_PREPROCESS_MAX_SAMPLES];
        let count = lossless_residual_shift_plan(&q, &mut shifts).unwrap();
        assert_eq!(count, 4);
        assert_eq!(shifts[0], 0);
        assert_eq!(shifts[1], 1);
        assert_eq!(shifts[2], 1);
        assert_eq!(shifts[3], 1);
    }

    #[test]
    fn shift_plan_caps_preprocessing_at_sixteen_samples() {
        let q = [0_i8; 60];
        let mut shifts = [255_u8; LOSSLESS_PREPROCESS_MAX_SAMPLES];
        assert_eq!(lossless_residual_shift_plan(&q, &mut shifts).unwrap(), 16);
        assert!(shifts.iter().all(|value| *value <= 7));
    }

    #[test]
    fn rejects_invalid_quantized_parcor_values() {
        let mut output = [0_u8; 1];
        assert!(lossless_residual_shift_plan(&[64_i8], &mut output).is_err());
        assert!(lossless_residual_shift_plan(&[-65_i8], &mut output).is_err());
    }
}
