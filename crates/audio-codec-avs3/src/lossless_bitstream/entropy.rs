use yinqidao_codec_core::CodecError;

use crate::bitreader::BitReader;

/// Width of the lossless entropy-coder selector carried in the coded bitstream.
pub const LOSSLESS_ENTROPY_MODE_BITS: u8 = 1;

/// Entropy decoder selected for one AVS lossless coded unit.
///
/// This enum describes the semantic value of the standardized one-bit selector. It deliberately
/// does not describe where that selector appears inside `ll_raw_data_block()`; the normative
/// frontend remains responsible for supplying the exact field offset and surrounding syntax.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LosslessEntropyMode {
    /// Selector value `0`: arithmetic entropy coding.
    Arithmetic,
    /// Selector value `1`: backward block-adaptive Golomb-Rice coding.
    GolombRice,
}

impl LosslessEntropyMode {
    #[inline]
    pub const fn from_selector_bit(bit: bool) -> Self {
        if bit {
            Self::GolombRice
        } else {
            Self::Arithmetic
        }
    }
}

/// Decode the standardized one-bit lossless entropy-coder selector at an arbitrary bit offset.
///
/// Returning the next bit position keeps this primitive composable with the eventual
/// `ll_raw_data_block()` parser without assuming any byte alignment or neighboring field widths.
pub fn decode_lossless_entropy_mode_at(
    bytes: &[u8],
    bit_offset: usize,
) -> Result<(LosslessEntropyMode, usize), CodecError> {
    let mut reader = BitReader::with_bit_position(bytes, bit_offset)?;
    let mode = LosslessEntropyMode::from_selector_bit(reader.read_bit()?);
    Ok((mode, reader.position_bits()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_both_standardized_entropy_modes() {
        let bytes = [0b0100_0000];
        assert_eq!(
            decode_lossless_entropy_mode_at(&bytes, 0).unwrap(),
            (LosslessEntropyMode::Arithmetic, 1)
        );
        assert_eq!(
            decode_lossless_entropy_mode_at(&bytes, 1).unwrap(),
            (LosslessEntropyMode::GolombRice, 2)
        );
    }

    #[test]
    fn selector_is_one_bit_and_supports_unaligned_offsets() {
        assert_eq!(LOSSLESS_ENTROPY_MODE_BITS, 1);
        let bytes = [0b1010_0000];
        assert_eq!(
            decode_lossless_entropy_mode_at(&bytes, 2).unwrap(),
            (LosslessEntropyMode::GolombRice, 3)
        );
        assert_eq!(
            decode_lossless_entropy_mode_at(&bytes, 3).unwrap(),
            (LosslessEntropyMode::Arithmetic, 4)
        );
    }

    #[test]
    fn rejects_selector_past_end_of_payload() {
        assert!(matches!(
            decode_lossless_entropy_mode_at(&[0], 8),
            Err(CodecError::Truncated)
        ));
    }
}
