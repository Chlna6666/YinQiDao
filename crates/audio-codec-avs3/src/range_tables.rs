use yinqidao_codec_core::CodecError;

use crate::{RANGE_DEFAULT_PRECISION, RangeModel};

mod b9_01_46;
mod b9_47_52;
mod b9_53_55;
mod b9_56_57;
mod b9_58_59;
mod b9_60;
mod b9_61;
mod b9_62;
mod b9_63;
mod b9_64;

pub const CONTEXT_RANGE_MODEL_COUNT: usize = 16;
pub const BASE_RANGE_MODEL_COUNT: usize = 64;

const CONTEXT_CDF_DEFAULT: [u32; 5] = [0, 1, 65_534, 65_535, 65_536];
const CONTEXT_CDF_5: [u32; 41] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 11, 16, 30, 69, 169, 405, 909, 1_885, 3_581, 6_203, 9_806,
    14_349, 20_045, 27_710, 39_581, 63_884, 65_352, 65_478, 65_513, 65_523, 65_526, 65_527, 65_528,
    65_529, 65_530, 65_531, 65_532, 65_533, 65_534, 65_535, 65_536,
];
const CONTEXT_CDF_13: [u32; 17] = [
    0, 1, 2, 3, 4, 16, 227, 4_637, 62_245, 65_417, 65_525, 65_531, 65_532, 65_533, 65_534, 65_535,
    65_536,
];

// GY/T 363-2023 table B.8. Rust has no hexadecimal floating-point literal syntax, therefore the
// normative binary32 values are stored by bit pattern to avoid decimal-rounding drift.
pub const BASE_STDDEV_THRESHOLD_BITS: [u32; BASE_RANGE_MODEL_COUNT] = [
    0x3DE147AE, 0x3DFEC794, 0x3E10122F, 0x3E22EFC6, 0x3E3845C8, 0x3E506705, 0x3E6BB124, 0x3E854709,
    0x3E96BACD, 0x3EAA779A, 0x3EC0CA0D, 0x3EDA08CA, 0x3EF695CB, 0x3F0B6FF2, 0x3F1DB233, 0x3F325888,
    0x3F49B317, 0x3F641C85, 0x3F80FDAB, 0x3F91E1BC, 0x3FA4FC06, 0x3FBA96AE, 0x3FD3058E, 0x3FEEA77B,
    0x4006F3DB, 0x40189FC6, 0x402C9C15, 0x4043365A, 0x405CC650, 0x4079AF2F, 0x408D3096, 0x409FAD96,
    0x40B4965C, 0x40CC3C0F, 0x40E6FA78, 0x41029CB2, 0x4113B71C, 0x41270EDD, 0x413CEF07, 0x4155AC84,
    0x4171A75A, 0x4188A611, 0x419A8AD8, 0x41AEC774, 0x41C5AA73, 0x41DF8CA8, 0x41FCD28B, 0x420EF6DD,
    0x4221AF5A, 0x4236DB67, 0x424ECD2F, 0x4269E1A4, 0x428440F1, 0x42959262, 0x42A9285F, 0x42BF4EEC,
    0x42D85C03, 0x42F4B0E0, 0x430A5DBC, 0x431C7C15, 0x4330F9CF, 0x43482670, 0x43625BEE, 0x43800000,
];

// GY/T 363-2023 table B.9 signed symbol offsets. Keep these explicit rather than deriving them
// from CDF length so table transcription errors cannot silently alter the reconstructed integer.
pub const BASE_RANGE_MODEL_OFFSETS: [i32; BASE_RANGE_MODEL_COUNT] = [
    -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -2, -2, -2, -2, -2, -3, -3, -3, -3, -4, -4, -5, -5, -6,
    -7, -7, -8, -9, -10, -12, -13, -15, -17, -19, -21, -24, -27, -31, -35, -39, -44, -50, -56, -64,
    -72, -81, -92, -104, -117, -132, -150, -169, -191, -216, -245, -277, -313, -354, -400, -452,
    -511, -578, -654, -739,
];

pub fn context_range_model(index: usize) -> Result<RangeModel<'static>, CodecError> {
    let cdf: &'static [u32] = match index {
        4 => &CONTEXT_CDF_5,
        12 => &CONTEXT_CDF_13,
        0..=15 => &CONTEXT_CDF_DEFAULT,
        _ => {
            return Err(CodecError::InvalidData(
                "context range-model index exceeds table B.1",
            ));
        }
    };
    RangeModel::new(
        cdf,
        centered_signed_offset(cdf.len())?,
        RANGE_DEFAULT_PRECISION,
    )
}

/// Return one exact table-B.9 base distribution.
///
/// `index` is zero-based: index 0 selects row 1 and index 63 selects row 64.
pub fn base_range_model(index: usize) -> Result<RangeModel<'static>, CodecError> {
    let cdf: &'static [u32] = match index {
        0..=45 => b9_01_46::ROWS[index],
        46..=51 => b9_47_52::ROWS[index - 46],
        52..=54 => b9_53_55::ROWS[index - 52],
        55..=56 => b9_56_57::ROWS[index - 55],
        57..=58 => b9_58_59::ROWS[index - 57],
        59 => b9_60::ROWS[0],
        60 => b9_61::ROWS[0],
        61 => b9_62::ROWS[0],
        62 => b9_63::ROWS[0],
        63 => b9_64::ROWS[0],
        _ => {
            return Err(CodecError::InvalidData(
                "base range-model index exceeds table B.9",
            ));
        }
    };
    RangeModel::new(
        cdf,
        BASE_RANGE_MODEL_OFFSETS[index],
        RANGE_DEFAULT_PRECISION,
    )
}

pub fn base_stddev_threshold(index: usize) -> Option<f32> {
    BASE_STDDEV_THRESHOLD_BITS
        .get(index)
        .copied()
        .map(f32::from_bits)
}

pub fn select_base_range_model_index(stddev: f32) -> Result<usize, CodecError> {
    if !stddev.is_finite() || stddev < 0.0 {
        return Err(CodecError::InvalidData(
            "base range-model standard deviation is invalid",
        ));
    }

    let mut low = 0_usize;
    let mut high = BASE_RANGE_MODEL_COUNT;
    while low < high {
        let mid = low + (high - low) / 2;
        let threshold = f32::from_bits(BASE_STDDEV_THRESHOLD_BITS[mid]);
        if threshold >= stddev {
            high = mid;
        } else {
            low = mid + 1;
        }
    }
    Ok(low.min(BASE_RANGE_MODEL_COUNT - 1))
}

fn centered_signed_offset(cdf_len: usize) -> Result<i32, CodecError> {
    if cdf_len < 3 || cdf_len & 1 == 0 {
        return Err(CodecError::InvalidData(
            "centered range CDF must have odd length >= 3",
        ));
    }
    let half_span = (cdf_len - 3) / 2;
    i32::try_from(half_span)
        .map(|value| -value)
        .map_err(|_| CodecError::InvalidData("range CDF signed offset exceeds i32 domain"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_context_models_satisfy_range_contract() {
        for index in 0..CONTEXT_RANGE_MODEL_COUNT {
            let model = context_range_model(index).unwrap();
            assert_eq!(model.precision(), 16);
            assert_eq!(model.cumulative().first(), Some(&0));
            assert_eq!(model.cumulative().last(), Some(&65_536));
        }
        assert_eq!(context_range_model(0).unwrap().offset(), -1);
        assert_eq!(context_range_model(4).unwrap().offset(), -19);
        assert_eq!(context_range_model(12).unwrap().offset(), -7);
        assert!(context_range_model(16).is_err());
    }

    #[test]
    fn all_b9_models_satisfy_range_contract_and_explicit_offsets() {
        for (index, expected_offset) in BASE_RANGE_MODEL_OFFSETS.iter().copied().enumerate() {
            let model = base_range_model(index).unwrap();
            assert_eq!(model.precision(), 16);
            assert_eq!(model.offset(), expected_offset);
            assert_eq!(model.cumulative().first(), Some(&0));
            assert_eq!(model.cumulative().last(), Some(&65_536));
            assert!(model.cumulative().windows(2).all(|pair| pair[0] < pair[1]));
        }
        assert!(base_range_model(BASE_RANGE_MODEL_COUNT).is_err());
    }

    #[test]
    fn b9_large_rows_keep_normative_shape_and_offsets() {
        let row_58 = base_range_model(57).unwrap();
        assert_eq!(row_58.cumulative().len(), 711);
        assert_eq!(row_58.offset(), -354);

        let row_60 = base_range_model(59).unwrap();
        assert_eq!(row_60.cumulative().len(), 907);
        assert_eq!(row_60.offset(), -452);

        let row_64 = base_range_model(63).unwrap();
        assert_eq!(row_64.cumulative().len(), 1_481);
        assert_eq!(row_64.offset(), -739);
    }

    #[test]
    fn b8_thresholds_are_strictly_increasing_and_exact_at_ends() {
        let mut previous = -1.0_f32;
        for index in 0..BASE_RANGE_MODEL_COUNT {
            let value = base_stddev_threshold(index).unwrap();
            assert!(value > previous);
            previous = value;
        }
        assert_eq!(base_stddev_threshold(0).unwrap().to_bits(), 0x3DE147AE);
        assert_eq!(base_stddev_threshold(63), Some(256.0));
        assert_eq!(base_stddev_threshold(64), None);
    }

    #[test]
    fn b8_selector_uses_lower_bound_and_clamps_high_tail() {
        let first = base_stddev_threshold(0).unwrap();
        let second = base_stddev_threshold(1).unwrap();
        assert_eq!(select_base_range_model_index(0.0).unwrap(), 0);
        assert_eq!(select_base_range_model_index(first).unwrap(), 0);
        assert_eq!(
            select_base_range_model_index((first + second) * 0.5).unwrap(),
            1
        );
        assert_eq!(select_base_range_model_index(256.0).unwrap(), 63);
        assert_eq!(select_base_range_model_index(1_000.0).unwrap(), 63);
        assert!(select_base_range_model_index(-0.01).is_err());
        assert!(select_base_range_model_index(f32::NAN).is_err());
    }
}
