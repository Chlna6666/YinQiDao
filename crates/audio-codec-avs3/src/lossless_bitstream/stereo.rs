use yinqidao_codec_core::CodecError;

use crate::lossless_primitives::restore_lossless_mid_side;

/// Semantic stereo decorrelation mode used by the AVS3 lossless inverse channel stage.
///
/// This deliberately does not encode the `ll_raw_data_block()` wire representation. The AVS3
/// lossless stereo syntax selects three logical reconstruction modes, but the bit width and
/// concrete flag parsing belong to the normative bitstream frontend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LosslessStereoDecorrelationMode {
    /// Near out-of-phase input: `Mid=floor((L-R)/2)`, `Sid=L+R`.
    AntiPhase,
    /// Near in-phase input: `Mid=floor((L+R)/2)`, `Sid=L-R`.
    InPhase,
    /// No decorrelation was applied; the transmitted channels are already left/right.
    Passthrough,
}

/// Restore the AVS3 lossless near-out-of-phase stereo transform.
///
/// For transmitted `Mid`/`Sid`, the normative inverse is:
///
/// ```text
/// parity = Sid & 1
/// L = Mid + (Sid + parity) / 2
/// R = (Sid - parity) / 2 - Mid
/// ```
///
/// `Sid +/- parity` is always even, so the divisions are exact for both positive and negative
/// odd values. Arithmetic is widened before narrowing back to the 32-bit PCM working domain.
#[inline]
pub fn restore_lossless_anti_phase(mid: i32, side: i32) -> Result<(i32, i32), CodecError> {
    let mid = i64::from(mid);
    let side = i64::from(side);
    let parity = side & 1;
    let half_up = (side + parity) / 2;
    let half_down = (side - parity) / 2;

    let left = mid.checked_add(half_up).ok_or(CodecError::InvalidData(
        "lossless anti-phase left reconstruction overflow",
    ))?;
    let right = half_down.checked_sub(mid).ok_or(CodecError::InvalidData(
        "lossless anti-phase right reconstruction overflow",
    ))?;

    Ok((
        i32::try_from(left)
            .map_err(|_| CodecError::InvalidData("lossless anti-phase left channel exceeds i32"))?,
        i32::try_from(right).map_err(|_| {
            CodecError::InvalidData("lossless anti-phase right channel exceeds i32")
        })?,
    ))
}

/// Restore one AVS3 lossless stereo sample pair after entropy/LPC/lifting reconstruction.
///
/// The caller supplies the already-decoded semantic mode. Numeric flag decoding is intentionally
/// kept out of this primitive until the normative `ll_raw_data_block()` syntax is wired.
#[inline]
pub fn restore_lossless_stereo_pair(
    mode: LosslessStereoDecorrelationMode,
    primary: i32,
    secondary: i32,
) -> Result<(i32, i32), CodecError> {
    match mode {
        LosslessStereoDecorrelationMode::AntiPhase => {
            restore_lossless_anti_phase(primary, secondary)
        }
        LosslessStereoDecorrelationMode::InPhase => restore_lossless_mid_side(primary, secondary),
        LosslessStereoDecorrelationMode::Passthrough => Ok((primary, secondary)),
    }
}

/// Restore planar AVS3 lossless stereo channels in place without allocation.
///
/// On return, `primary` contains the left channel and `secondary` contains the right channel.
pub fn restore_lossless_stereo_in_place(
    mode: LosslessStereoDecorrelationMode,
    primary: &mut [i32],
    secondary: &mut [i32],
) -> Result<(), CodecError> {
    if primary.len() != secondary.len() {
        return Err(CodecError::InvalidData(
            "lossless stereo channel lengths do not match",
        ));
    }

    if mode == LosslessStereoDecorrelationMode::Passthrough {
        return Ok(());
    }

    for (primary_sample, secondary_sample) in primary.iter_mut().zip(secondary.iter_mut()) {
        let (left, right) = restore_lossless_stereo_pair(mode, *primary_sample, *secondary_sample)?;
        *primary_sample = left;
        *secondary_sample = right;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anti_phase_downmix(left: i32, right: i32) -> (i32, i32) {
        let left = i64::from(left);
        let right = i64::from(right);
        (
            i32::try_from((left - right).div_euclid(2)).unwrap(),
            i32::try_from(left + right).unwrap(),
        )
    }

    fn in_phase_downmix(left: i32, right: i32) -> (i32, i32) {
        let left = i64::from(left);
        let right = i64::from(right);
        (
            i32::try_from((left + right).div_euclid(2)).unwrap(),
            i32::try_from(left - right).unwrap(),
        )
    }

    #[test]
    fn anti_phase_inverse_preserves_signed_odd_parity() {
        for (left, right) in [
            (0, 1),
            (1, 0),
            (-1, 0),
            (0, -1),
            (-2, 3),
            (3, -2),
            (-5, -2),
            (5, 2),
            (123, -456),
        ] {
            let (mid, side) = anti_phase_downmix(left, right);
            assert_eq!(
                restore_lossless_anti_phase(mid, side).unwrap(),
                (left, right)
            );
        }
    }

    #[test]
    fn in_phase_mode_matches_existing_mid_side_inverse() {
        for (left, right) in [
            (0, 1),
            (1, 0),
            (-1, 0),
            (0, -1),
            (-2, 3),
            (3, -2),
            (-5, -2),
            (5, 2),
            (123, -456),
        ] {
            let (mid, side) = in_phase_downmix(left, right);
            let expected = restore_lossless_mid_side(mid, side).unwrap();
            assert_eq!(
                restore_lossless_stereo_pair(LosslessStereoDecorrelationMode::InPhase, mid, side,)
                    .unwrap(),
                expected
            );
            assert_eq!(expected, (left, right));
        }
    }

    #[test]
    fn passthrough_mode_preserves_transmitted_channels() {
        assert_eq!(
            restore_lossless_stereo_pair(LosslessStereoDecorrelationMode::Passthrough, -123, 456,)
                .unwrap(),
            (-123, 456)
        );
    }

    #[test]
    fn planar_stereo_reconstruction_is_allocation_free_and_exact() {
        let original = [(0, 1), (-2, 3), (5, 2), (123, -456)];
        let mut primary = Vec::new();
        let mut secondary = Vec::new();
        for &(left, right) in &original {
            let (mid, side) = anti_phase_downmix(left, right);
            primary.push(mid);
            secondary.push(side);
        }

        restore_lossless_stereo_in_place(
            LosslessStereoDecorrelationMode::AntiPhase,
            &mut primary,
            &mut secondary,
        )
        .unwrap();

        assert_eq!(
            primary,
            original.iter().map(|&(left, _)| left).collect::<Vec<_>>()
        );
        assert_eq!(
            secondary,
            original.iter().map(|&(_, right)| right).collect::<Vec<_>>()
        );
    }

    #[test]
    fn stereo_reconstruction_rejects_invalid_geometry_and_overflow() {
        let mut primary = [0, 1];
        let mut secondary = [0];
        assert!(
            restore_lossless_stereo_in_place(
                LosslessStereoDecorrelationMode::AntiPhase,
                &mut primary,
                &mut secondary,
            )
            .is_err()
        );

        assert!(restore_lossless_anti_phase(i32::MAX, i32::MAX).is_err());
        assert!(
            restore_lossless_stereo_pair(
                LosslessStereoDecorrelationMode::InPhase,
                i32::MAX,
                i32::MAX,
            )
            .is_err()
        );
    }
}
