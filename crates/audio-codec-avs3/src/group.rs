use yinqidao_codec_core::CodecError;

use crate::{TransformType, bitreader::BitReader};

const SHORT_BLOCKS: usize = 8;

/// Table 17 `DecodeGroupBits()` result for one coded channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GroupSideInfo {
    pub num_groups: u8,
    pub group_indicator: [bool; SHORT_BLOCKS],
    pub next_bit_offset: usize,
}

/// Parse one channel's spectrum inverse-grouping syntax.
///
/// Only short-window frames carry bits. `numGroups` is transmitted as one bit and incremented by
/// one; when it resolves to two, all eight short blocks carry a one-bit group indicator. Long,
/// cut-in and cut-out windows consume no grouping bits and always use one group.
pub fn parse_group_bits_at(
    bytes: &[u8],
    bit_offset: usize,
    transform_type: TransformType,
) -> Result<GroupSideInfo, CodecError> {
    if transform_type != TransformType::Short {
        return Ok(GroupSideInfo {
            num_groups: 1,
            group_indicator: [false; SHORT_BLOCKS],
            next_bit_offset: bit_offset,
        });
    }

    let mut reader = BitReader::with_bit_position(bytes, bit_offset)?;
    let num_groups = (reader.read_bit()? as u8) + 1;
    let mut group_indicator = [false; SHORT_BLOCKS];
    if num_groups == 2 {
        for indicator in &mut group_indicator {
            *indicator = reader.read_bit()?;
        }
    }

    Ok(GroupSideInfo {
        num_groups,
        group_indicator,
        next_bit_offset: reader.position_bits(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_short_windows_consume_no_bits() {
        for transform in [TransformType::Long, TransformType::CutIn, TransformType::CutOut] {
            let info = parse_group_bits_at(&[], 13, transform).unwrap();
            assert_eq!(info.num_groups, 1);
            assert_eq!(info.group_indicator, [false; 8]);
            assert_eq!(info.next_bit_offset, 13);
        }
    }

    #[test]
    fn short_single_group_consumes_one_bit() {
        let info = parse_group_bits_at(&[0], 0, TransformType::Short).unwrap();
        assert_eq!(info.num_groups, 1);
        assert_eq!(info.next_bit_offset, 1);
        assert_eq!(info.group_indicator, [false; 8]);
    }

    #[test]
    fn short_two_groups_consumes_all_eight_indicators() {
        // numGroups raw=1 then 8 indicators: 10110010.
        let bytes = [0b1_1011001, 0b0_0000000];
        let info = parse_group_bits_at(&bytes, 0, TransformType::Short).unwrap();
        assert_eq!(info.num_groups, 2);
        assert_eq!(
            info.group_indicator,
            [true, false, true, true, false, false, true, false]
        );
        assert_eq!(info.next_bit_offset, 9);
    }

    #[test]
    fn short_frame_rejects_truncated_indicator_vector() {
        assert_eq!(
            parse_group_bits_at(&[0b1000_0000], 0, TransformType::Short),
            Err(CodecError::Truncated)
        );
    }
}
