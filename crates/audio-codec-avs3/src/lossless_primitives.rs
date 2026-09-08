use yinqidao_codec_core::CodecError;

/// Fixed moving-window length used by the GB/T 33475.3-2018 backward block-adaptive Rice method.
pub const LOSSLESS_RICE_WINDOW: usize = 32;
/// Normative sub-block geometries used by the lossless Rice path.
pub const LOSSLESS_RICE_BLOCK_SIZES: [usize; 5] = [2, 4, 8, 16, 32];
/// Maximum unary-prefix quotient before the encoder raises the effective Rice parameter.
pub const LOSSLESS_RICE_MAX_PREFIX: u64 = 63;

/// Map one signed lossless residual onto the non-negative Rice alphabet.
///
/// This is the exact mapping defined by the backward block-adaptive Golomb-Rice method:
/// non-negative `x -> 2*x`, negative `x -> -2*x-1`.
#[inline]
pub fn lossless_rice_map_signed(value: i32) -> u64 {
    if value >= 0 {
        (value as u64) << 1
    } else {
        ((-i64::from(value)) as u64) * 2 - 1
    }
}

/// Inverse of [`lossless_rice_map_signed`].
#[inline]
pub fn lossless_rice_unmap_signed(value: u64) -> Result<i32, CodecError> {
    if value > u64::from(u32::MAX) {
        return Err(CodecError::InvalidData(
            "lossless Rice symbol exceeds signed 32-bit residual range",
        ));
    }
    let signed = if value & 1 == 0 {
        i64::try_from(value >> 1)
            .map_err(|_| CodecError::InvalidData("lossless Rice symbol conversion overflow"))?
    } else {
        -i64::try_from((value >> 1) + 1)
            .map_err(|_| CodecError::InvalidData("lossless Rice symbol conversion overflow"))?
    };
    i32::try_from(signed)
        .map_err(|_| CodecError::InvalidData("lossless Rice residual exceeds i32 range"))
}

/// Split one unsigned symbol into the unary quotient and fixed-width Rice remainder.
#[inline]
pub fn lossless_rice_split(value: u64, parameter: u8) -> Result<(u64, u64), CodecError> {
    if parameter > 63 {
        return Err(CodecError::InvalidData(
            "lossless Rice parameter exceeds 64-bit symbol geometry",
        ));
    }
    if parameter == 64 {
        unreachable!("parameter > 63 rejected above");
    }
    let quotient = value >> parameter;
    let remainder = if parameter == 0 {
        0
    } else {
        value & ((1_u64 << parameter) - 1)
    };
    Ok((quotient, remainder))
}

/// Raise `parameter` only as far as needed to keep the unary quotient at or below 63.
///
/// The lossless bitstream carries this temporary increment after the Rice separator when the
/// original quotient would exceed the normative prefix limit. This helper deliberately does not
/// parse that syntax; it provides the exact arithmetic needed by the eventual bitstream frontend.
pub fn lossless_rice_escape_parameter(
    value: u64,
    parameter: u8,
) -> Result<(u8, u8), CodecError> {
    if parameter > 63 {
        return Err(CodecError::InvalidData(
            "lossless Rice parameter exceeds 64-bit symbol geometry",
        ));
    }
    let mut effective = parameter;
    while effective < 63 && (value >> effective) > LOSSLESS_RICE_MAX_PREFIX {
        effective += 1;
    }
    if (value >> effective) > LOSSLESS_RICE_MAX_PREFIX {
        return Err(CodecError::InvalidData(
            "lossless Rice symbol cannot satisfy unary-prefix limit",
        ));
    }
    Ok((effective, effective - parameter))
}

/// Validate the five sub-block sizes used by the 32-value backward-adaptation window.
#[inline]
pub fn validate_lossless_rice_block_size(block_size: usize) -> Result<(), CodecError> {
    if LOSSLESS_RICE_BLOCK_SIZES.contains(&block_size) {
        Ok(())
    } else {
        Err(CodecError::InvalidData(
            "lossless Rice block size must be 2, 4, 8, 16, or 32",
        ))
    }
}

/// Decoder-side rolling statistic for backward block-adaptive Golomb-Rice coding.
///
/// `sum` is initialized as `2^m * 32`. For an already unsigned-mapped residual sub-block the
/// patent's update equation subtracts `sum/32` once per symbol and adds that symbol's mapped
/// magnitude. Parameter recentering chooses the largest permitted correction, i.e. the parameter
/// whose `[2^m*32, 2^(m+1)*32]` interval contains the updated statistic. The syntax layer may choose
/// a smaller permitted correction; keeping this arithmetic isolated prevents that syntax decision
/// from leaking into signed mapping or channel reconstruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LosslessRiceState {
    parameter: u8,
    sum: u64,
    block_size: usize,
}

impl LosslessRiceState {
    pub fn new(parameter: u8, block_size: usize) -> Result<Self, CodecError> {
        if parameter > 57 {
            // 2^m * 32 must remain representable in u64.
            return Err(CodecError::InvalidData(
                "lossless Rice initial parameter overflows 32-value window sum",
            ));
        }
        validate_lossless_rice_block_size(block_size)?;
        Ok(Self {
            parameter,
            sum: (1_u64 << parameter) * LOSSLESS_RICE_WINDOW as u64,
            block_size,
        })
    }

    pub fn parameter(self) -> u8 {
        self.parameter
    }

    pub fn sum(self) -> u64 {
        self.sum
    }

    pub fn block_size(self) -> usize {
        self.block_size
    }

    /// Update the 32-value rolling statistic from one unsigned-mapped sub-block.
    pub fn update_mapped_block(&mut self, values: &[u64]) -> Result<(), CodecError> {
        if values.len() != self.block_size {
            return Err(CodecError::InvalidData(
                "lossless Rice sub-block length does not match configured block size",
            ));
        }
        let decay_per_symbol = self.sum / LOSSLESS_RICE_WINDOW as u64;
        let decay = decay_per_symbol
            .checked_mul(values.len() as u64)
            .ok_or(CodecError::InvalidData(
                "lossless Rice rolling-statistic decay overflow",
            ))?;
        let block_sum = values.iter().try_fold(0_u64, |sum, &value| {
            sum.checked_add(value).ok_or(CodecError::InvalidData(
                "lossless Rice mapped sub-block sum overflow",
            ))
        })?;
        self.sum = self
            .sum
            .checked_add(block_sum)
            .and_then(|value| value.checked_sub(decay))
            .ok_or(CodecError::InvalidData(
                "lossless Rice rolling-statistic update overflow",
            ))?;
        Ok(())
    }

    /// Apply the maximal parameter correction allowed by the rolling statistic.
    pub fn recenter_parameter(&mut self) {
        loop {
            let lower = (1_u128 << self.parameter) * LOSSLESS_RICE_WINDOW as u128;
            let upper = if self.parameter == 63 {
                u128::MAX
            } else {
                (1_u128 << (self.parameter + 1)) * LOSSLESS_RICE_WINDOW as u128
            };
            let sum = u128::from(self.sum);
            if sum > upper && self.parameter < 63 {
                self.parameter += 1;
            } else if sum < lower && self.parameter > 0 {
                self.parameter -= 1;
            } else {
                break;
            }
        }
    }
}

/// Restore one losslessly decorrelated channel pair from its transmitted Mid/Side integers.
///
/// The odd/even compensation is essential: a plain `mid +/- side/2` loses one LSB for odd Side.
/// Arithmetic is widened before returning to `i32`, so malformed streams fail instead of wrapping.
#[inline]
pub fn restore_lossless_mid_side(mid: i32, side: i32) -> Result<(i32, i32), CodecError> {
    let mid = i64::from(mid);
    let side = i64::from(side);
    let parity = side & 1;
    let left = mid
        .checked_add((side + parity) / 2)
        .ok_or(CodecError::InvalidData(
            "lossless channel reconstruction overflow",
        ))?;
    let right = mid
        .checked_sub((side - parity) / 2)
        .ok_or(CodecError::InvalidData(
            "lossless channel reconstruction overflow",
        ))?;
    Ok((
        i32::try_from(left).map_err(|_| {
            CodecError::InvalidData("lossless reconstructed left channel exceeds i32")
        })?,
        i32::try_from(right).map_err(|_| {
            CodecError::InvalidData("lossless reconstructed right channel exceeds i32")
        })?,
    ))
}

/// Restore one planar decorrelated pair in place without allocation.
pub fn restore_lossless_mid_side_in_place(
    mid: &mut [i32],
    side: &mut [i32],
) -> Result<(), CodecError> {
    if mid.len() != side.len() {
        return Err(CodecError::InvalidData(
            "lossless Mid/Side channel lengths do not match",
        ));
    }
    for (mid_sample, side_sample) in mid.iter_mut().zip(side.iter_mut()) {
        let (left, right) = restore_lossless_mid_side(*mid_sample, *side_sample)?;
        *mid_sample = left;
        *side_sample = right;
    }
    Ok(())
}

/// Merge the even and odd branches produced by a completed inverse lifting stage.
pub fn merge_lossless_lifting_branches(
    even: &[i32],
    odd: &[i32],
    output: &mut [i32],
) -> Result<(), CodecError> {
    if even.len() != odd.len() || output.len() != even.len().saturating_mul(2) {
        return Err(CodecError::InvalidData(
            "lossless inverse-lifting branch geometry mismatch",
        ));
    }
    for (index, (&even_value, &odd_value)) in even.iter().zip(odd).enumerate() {
        output[2 * index] = even_value;
        output[2 * index + 1] = odd_value;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_rice_mapping_round_trips_entire_edge_geometry() {
        for value in [
            i32::MIN,
            -1_000_000,
            -2,
            -1,
            0,
            1,
            2,
            1_000_000,
            i32::MAX,
        ] {
            let mapped = lossless_rice_map_signed(value);
            assert_eq!(lossless_rice_unmap_signed(mapped).unwrap(), value);
        }
        assert_eq!(lossless_rice_map_signed(0), 0);
        assert_eq!(lossless_rice_map_signed(-1), 1);
        assert_eq!(lossless_rice_map_signed(1), 2);
    }

    #[test]
    fn rice_escape_raises_parameter_only_until_prefix_fits() {
        let value = 1_u64 << 20;
        let (effective, increment) = lossless_rice_escape_parameter(value, 4).unwrap();
        assert_eq!(effective, 15);
        assert_eq!(increment, 11);
        assert!((value >> effective) <= LOSSLESS_RICE_MAX_PREFIX);
        assert!((value >> (effective - 1)) > LOSSLESS_RICE_MAX_PREFIX);
    }

    #[test]
    fn rice_state_uses_normative_window_and_block_sizes() {
        let mut state = LosslessRiceState::new(3, 4).unwrap();
        assert_eq!(state.sum(), 256);
        state.update_mapped_block(&[8, 8, 8, 8]).unwrap();
        // sum + block_sum - block_size * (sum/32) = 256 + 32 - 4*8 = 256.
        assert_eq!(state.sum(), 256);
        state.recenter_parameter();
        assert_eq!(state.parameter(), 3);
        assert!(LosslessRiceState::new(3, 3).is_err());
    }

    #[test]
    fn mid_side_reconstruction_preserves_odd_side_lsb() {
        assert_eq!(restore_lossless_mid_side(1, 1).unwrap(), (2, 1));
        assert_eq!(restore_lossless_mid_side(1, -1).unwrap(), (1, 2));
        assert_eq!(restore_lossless_mid_side(-2, -1).unwrap(), (-2, -1));
        assert_eq!(restore_lossless_mid_side(-2, 1).unwrap(), (-1, -2));
    }

    #[test]
    fn planar_mid_side_reconstruction_is_allocation_free_and_exact() {
        let mut mid = [1, 1, -2, -2];
        let mut side = [1, -1, -1, 1];
        restore_lossless_mid_side_in_place(&mut mid, &mut side).unwrap();
        assert_eq!(mid, [2, 1, -2, -1]);
        assert_eq!(side, [1, 2, -1, -2]);
    }

    #[test]
    fn inverse_lifting_merge_restores_interleaving() {
        let even = [10, 20, 30];
        let odd = [-1, -2, -3];
        let mut output = [0; 6];
        merge_lossless_lifting_branches(&even, &odd, &mut output).unwrap();
        assert_eq!(output, [10, -1, 20, -2, 30, -3]);
    }
}
