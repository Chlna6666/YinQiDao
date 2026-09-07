use std::f32::consts::FRAC_1_SQRT_2;

use yinqidao_codec_core::CodecError;

use crate::{StereoCouplingSideInfo, StereoSideInfo};

/// Shared orthonormal inverse M/S primitive used by stereo and multichannel MCAC upmixing.
///
/// Each frequency line is independent, so both inputs may be overwritten in place without scratch.
pub(crate) fn inverse_ms_pair(
    channel_zero: &mut [f32],
    channel_one: &mut [f32],
) -> Result<(), CodecError> {
    if channel_zero.len() != channel_one.len() {
        return Err(CodecError::InvalidData(
            "inverse M/S spectra must have identical lengths",
        ));
    }
    for (mid, side) in channel_zero.iter_mut().zip(channel_one.iter_mut()) {
        let m = *mid;
        let s = *side;
        *mid = FRAC_1_SQRT_2 * (m + s);
        *side = FRAC_1_SQRT_2 * (m - s);
    }
    Ok(())
}

/// Apply the normative >32-kb/s dual-channel stereo inverse M/S and inverse ILD transform in place.
///
/// `channel_zero`/`channel_one` are the two downmixed spectra produced by inverse QC. When `isMs`
/// is false they already represent L/R and are left untouched. When it is true they represent M/S;
/// formulas (8)/(9) recover L/R and formula (10) restores the encoded inter-channel level ratio.
/// No scratch buffer is required because each frequency line depends only on the corresponding M/S
/// pair, so both input slices may be overwritten immediately.
pub fn apply_stereo_ms_upmix(
    side: StereoSideInfo,
    channel_zero: &mut [f32],
    channel_one: &mut [f32],
) -> Result<(), CodecError> {
    if channel_zero.len() != channel_one.len() {
        return Err(CodecError::InvalidData(
            "stereo upmix spectra must have identical lengths",
        ));
    }

    let StereoCouplingSideInfo::Ms {
        is_ms,
        ild_q_idx,
        ..
    } = side.coupling
    else {
        return Err(CodecError::Unsupported(
            "MCR stereo upmix requires GB/T 33475.3 B.154/B.155",
        ));
    };

    if !is_ms {
        return Ok(());
    }
    let ild = ild_q_idx.ok_or(CodecError::InvalidData(
        "M/S stereo frame is missing its ILD index",
    ))?;
    if !(1..=15).contains(&ild) {
        return Err(CodecError::InvalidData(
            "M/S stereo ILD index must be in 1..=15",
        ));
    }

    inverse_ms_pair(channel_zero, channel_one)?;

    // Annex D.10/D.11 quantizes L/(L+R) to 1..15. Algebraically the decoder's formula (10)
    // recovers the amplitude ratio as 16 / IldQIdx - 1.
    let level_ratio = 16.0 / f32::from(ild) - 1.0;
    if !level_ratio.is_finite() || level_ratio <= 0.0 {
        return Err(CodecError::InvalidData("invalid stereo inverse-ILD ratio"));
    }

    if level_ratio > 1.0 {
        for right in channel_one {
            *right *= level_ratio;
        }
    } else if level_ratio < 1.0 {
        let left_scale = level_ratio.recip();
        for left in channel_zero {
            *left *= left_scale;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms_side(ild: u8) -> StereoSideInfo {
        StereoSideInfo {
            coupling: StereoCouplingSideInfo::Ms {
                is_ms: true,
                ild_q_idx: Some(ild),
                bits_ratio: 4,
            },
            next_bit_offset: 0,
        }
    }

    #[test]
    fn shared_inverse_ms_primitive_is_orthonormal() {
        let mut mid = [1.0_f32, 1.0];
        let mut side = [1.0_f32, -1.0];
        inverse_ms_pair(&mut mid, &mut side).unwrap();
        assert!((mid[0] - 2.0_f32.sqrt()).abs() < 1.0e-6);
        assert!(side[0].abs() < 1.0e-6);
        assert!(mid[1].abs() < 1.0e-6);
        assert!((side[1] - 2.0_f32.sqrt()).abs() < 1.0e-6);
    }

    #[test]
    fn equal_ild_performs_orthonormal_inverse_ms_only() {
        let mut mid = [1.0_f32, 1.0];
        let mut side = [1.0_f32, -1.0];
        apply_stereo_ms_upmix(ms_side(8), &mut mid, &mut side).unwrap();
        assert!((mid[0] - 2.0_f32.sqrt()).abs() < 1.0e-6);
        assert!(side[0].abs() < 1.0e-6);
        assert!(mid[1].abs() < 1.0e-6);
        assert!((side[1] - 2.0_f32.sqrt()).abs() < 1.0e-6);
    }

    #[test]
    fn inverse_ild_expands_right_when_encoder_index_is_below_half() {
        let mut mid = [1.0_f32];
        let mut side = [0.0_f32];
        apply_stereo_ms_upmix(ms_side(4), &mut mid, &mut side).unwrap();
        let base = FRAC_1_SQRT_2;
        assert!((mid[0] - base).abs() < 1.0e-6);
        assert!((side[0] - base * 3.0).abs() < 1.0e-6);
    }

    #[test]
    fn inverse_ild_expands_left_when_encoder_index_is_above_half() {
        let mut mid = [1.0_f32];
        let mut side = [0.0_f32];
        apply_stereo_ms_upmix(ms_side(12), &mut mid, &mut side).unwrap();
        let base = FRAC_1_SQRT_2;
        assert!((mid[0] - base * 3.0).abs() < 1.0e-5);
        assert!((side[0] - base).abs() < 1.0e-6);
    }

    #[test]
    fn non_ms_frame_is_bit_exact_noop() {
        let side_info = StereoSideInfo {
            coupling: StereoCouplingSideInfo::Ms {
                is_ms: false,
                ild_q_idx: None,
                bits_ratio: 4,
            },
            next_bit_offset: 0,
        };
        let mut left = [1.0_f32, -2.0];
        let mut right = [3.0_f32, -4.0];
        let expected_left = left;
        let expected_right = right;
        apply_stereo_ms_upmix(side_info, &mut left, &mut right).unwrap();
        assert_eq!(left, expected_left);
        assert_eq!(right, expected_right);
    }
}
