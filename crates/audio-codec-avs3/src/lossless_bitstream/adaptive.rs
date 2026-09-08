use yinqidao_codec_core::CodecError;

use crate::lossless_primitives::{
    LOSSLESS_RICE_WINDOW, lossless_rice_map_signed, validate_lossless_rice_block_size,
};

/// Decoder-side adaptation state for the normative backward block-adaptive Golomb-Rice path.
///
/// The 2018 lossless tool uses a 32-sample moving statistic but does not account for predictor
/// seed samples and prediction residuals in the same domain. Seed samples contribute their
/// transmitted non-negative statistic directly; predicted samples contribute the unsigned
/// Golomb-Rice mapping of their signed residual. Keeping those paths separate prevents the
/// `ll_raw_data_block()` frontend from accidentally double-mapping predictor seeds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LosslessRiceAdaptationState {
    parameter: u8,
    sum: u64,
    block_size: usize,
}

impl LosslessRiceAdaptationState {
    pub(crate) fn new(parameter: u8, block_size: usize) -> Result<Self, CodecError> {
        if parameter > 57 {
            return Err(CodecError::InvalidData(
                "lossless Rice initial parameter overflows 32-sample adaptation sum",
            ));
        }
        validate_lossless_rice_block_size(block_size)?;
        Ok(Self {
            parameter,
            sum: (1_u64 << parameter) * LOSSLESS_RICE_WINDOW as u64,
            block_size,
        })
    }

    #[inline]
    pub(crate) const fn parameter(self) -> u8 {
        self.parameter
    }

    #[inline]
    pub(crate) const fn sum(self) -> u64 {
        self.sum
    }

    #[inline]
    pub(crate) const fn block_size(self) -> usize {
        self.block_size
    }

    /// Update the moving statistic for predictor seed samples.
    ///
    /// The caller supplies the exact non-negative statistic decoded for the seed region. The
    /// syntax layer owns how those values are represented on the wire; this state machine owns
    /// only the normative backward adaptation arithmetic.
    pub(crate) fn update_seed_block(&mut self, values: &[u64]) -> Result<(), CodecError> {
        self.update_terms(values.len(), values.iter().copied())
    }

    /// Update the moving statistic for signed prediction residuals without an intermediate buffer.
    pub(crate) fn update_residual_block(&mut self, residuals: &[i32]) -> Result<(), CodecError> {
        self.update_terms(
            residuals.len(),
            residuals.iter().copied().map(lossless_rice_map_signed),
        )
    }

    /// Update from residuals that have already been mapped onto the non-negative Rice alphabet.
    pub(crate) fn update_mapped_residual_block(
        &mut self,
        mapped: &[u64],
    ) -> Result<(), CodecError> {
        self.update_terms(mapped.len(), mapped.iter().copied())
    }

    fn update_terms(
        &mut self,
        count: usize,
        mut terms: impl Iterator<Item = u64>,
    ) -> Result<(), CodecError> {
        if count != self.block_size {
            return Err(CodecError::InvalidData(
                "lossless Rice adaptation block length does not match configured block size",
            ));
        }

        let decay_per_symbol = self.sum / LOSSLESS_RICE_WINDOW as u64;
        let decay = decay_per_symbol
            .checked_mul(count as u64)
            .ok_or(CodecError::InvalidData(
                "lossless Rice adaptation decay overflow",
            ))?;
        let block_sum = terms.try_fold(0_u64, |sum, value| {
            sum.checked_add(value).ok_or(CodecError::InvalidData(
                "lossless Rice adaptation block sum overflow",
            ))
        })?;

        self.sum = self
            .sum
            .checked_add(block_sum)
            .and_then(|value| value.checked_sub(decay))
            .ok_or(CodecError::InvalidData(
                "lossless Rice adaptation statistic overflow",
            ))?;
        Ok(())
    }

    /// Apply the maximal deterministic parameter correction permitted by the current statistic.
    ///
    /// The patent permits slower one-step correction as well as the maximal correction. The
    /// current AVS2 lossless bitstream frontend will choose the exact standardized policy once the
    /// corresponding `ll_raw_data_block()` field is parsed; this helper intentionally exposes the
    /// maximal arithmetic without silently assuming that policy in the parser.
    pub(crate) fn recenter_maximal(&mut self) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_and_residual_paths_use_different_domains() {
        let mut seed = LosslessRiceAdaptationState::new(3, 4).unwrap();
        seed.update_seed_block(&[8, 8, 8, 8]).unwrap();
        assert_eq!(seed.sum(), 256);

        let mut residual = LosslessRiceAdaptationState::new(3, 4).unwrap();
        residual.update_residual_block(&[4, -4, 4, -4]).unwrap();
        // Signed mapping is [8, 7, 8, 7], so 256 + 30 - 4*(256/32) = 254.
        assert_eq!(residual.sum(), 254);
    }

    #[test]
    fn mapped_residual_update_is_allocation_free_equivalent() {
        let mut signed = LosslessRiceAdaptationState::new(2, 2).unwrap();
        signed.update_residual_block(&[-3, 5]).unwrap();

        let mut mapped = LosslessRiceAdaptationState::new(2, 2).unwrap();
        mapped.update_mapped_residual_block(&[5, 10]).unwrap();
        assert_eq!(signed, mapped);
    }

    #[test]
    fn maximal_recentering_finds_the_matching_power_of_two_interval() {
        let mut state = LosslessRiceAdaptationState::new(1, 2).unwrap();
        state.update_seed_block(&[256, 256]).unwrap();
        state.recenter_maximal();
        assert!(state.parameter() > 1);
        let lower = (1_u128 << state.parameter()) * LOSSLESS_RICE_WINDOW as u128;
        let upper = if state.parameter() == 63 {
            u128::MAX
        } else {
            (1_u128 << (state.parameter() + 1)) * LOSSLESS_RICE_WINDOW as u128
        };
        assert!(u128::from(state.sum()) >= lower);
        assert!(u128::from(state.sum()) <= upper);
    }
}
