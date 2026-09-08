use yinqidao_codec_core::CodecError;

use crate::{TransformType, bitreader::BitReader};

const SHORT_BLOCKS: usize = 8;
const MDCT_LINES: usize = 1024;
const SHORT_LINES: usize = MDCT_LINES / SHORT_BLOCKS;

/// Table 17 `DecodeGroupBits()` result for one coded channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GroupSideInfo {
    pub num_groups: u8,
    pub group_indicator: [bool; SHORT_BLOCKS],
    pub next_bit_offset: usize,
}

/// Reusable scratch for short-window spectrum inverse grouping.
#[derive(Debug)]
pub struct SpectrumDegroupWorkspace {
    reordered: [f32; MDCT_LINES],
}

impl SpectrumDegroupWorkspace {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for SpectrumDegroupWorkspace {
    fn default() -> Self {
        Self {
            reordered: [0.0; MDCT_LINES],
        }
    }
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

/// Restore the normative short-window MDCT ordering after neural inverse-QC.
///
/// For two groups, the coded spectrum contains one independently interleaved transient group
/// followed by one independently interleaved non-transient group. This routine maps both groups
/// directly into the ordinary eight-block frequency-interleaved layout expected by subsequent
/// upmix/BWE/TNS processing. Long and transition windows are already in the required layout.
///
/// A single reusable 1024-float scratch buffer is sufficient; there is no heap allocation and no
/// intermediate per-group tensor.
pub fn inverse_group_spectrum(
    transform_type: TransformType,
    group: GroupSideInfo,
    spectrum: &mut [f32],
    workspace: &mut SpectrumDegroupWorkspace,
) -> Result<(), CodecError> {
    if transform_type != TransformType::Short {
        return Ok(());
    }
    if spectrum.len() != MDCT_LINES {
        return Err(CodecError::InvalidData(
            "short-window inverse grouping requires a 1024-line MDCT spectrum",
        ));
    }
    if !(1..=2).contains(&group.num_groups) {
        return Err(CodecError::InvalidData(
            "short-window inverse grouping requires one or two groups",
        ));
    }

    let transient_blocks = group
        .group_indicator
        .iter()
        .filter(|&&other| !other)
        .count();
    let other_blocks = SHORT_BLOCKS - transient_blocks;

    if group.num_groups == 1 {
        if other_blocks != 0 {
            return Err(CodecError::InvalidData(
                "single-group short frame must mark all blocks as the transient group",
            ));
        }
    } else if transient_blocks == 0 || other_blocks == 0 {
        return Err(CodecError::InvalidData(
            "two-group short frame must contain both transient and non-transient blocks",
        ));
    }

    let mut transient_rank = 0usize;
    let mut other_rank = 0usize;
    for block in 0..SHORT_BLOCKS {
        let (group_start, group_width, rank) = if group.group_indicator[block] {
            let rank = other_rank;
            other_rank += 1;
            (transient_blocks * SHORT_LINES, other_blocks, rank)
        } else {
            let rank = transient_rank;
            transient_rank += 1;
            (0, transient_blocks, rank)
        };

        for line in 0..SHORT_LINES {
            let source = group_start + rank + group_width * line;
            let destination = block + SHORT_BLOCKS * line;
            workspace.reordered[destination] = spectrum[source];
        }
    }

    spectrum.copy_from_slice(&workspace.reordered);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_short_windows_consume_no_bits() {
        for transform in [
            TransformType::Long,
            TransformType::CutIn,
            TransformType::CutOut,
        ] {
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

    #[test]
    fn single_group_inverse_mapping_is_bit_exact_identity() {
        let group = GroupSideInfo {
            num_groups: 1,
            group_indicator: [false; SHORT_BLOCKS],
            next_bit_offset: 0,
        };
        let input: [f32; MDCT_LINES] = std::array::from_fn(|index| index as f32 - 512.0);
        let mut spectrum = input;
        let mut workspace = SpectrumDegroupWorkspace::new();
        inverse_group_spectrum(TransformType::Short, group, &mut spectrum, &mut workspace).unwrap();
        assert_eq!(spectrum, input);
    }

    #[test]
    fn two_group_inverse_mapping_restores_temporal_block_order() {
        let indicators = [true, true, true, false, false, false, true, true];
        let group = GroupSideInfo {
            num_groups: 2,
            group_indicator: indicators,
            next_bit_offset: 0,
        };
        let transient_blocks = 3usize;
        let other_blocks = 5usize;
        let mut grouped = [0.0_f32; MDCT_LINES];

        let mut transient_rank = 0usize;
        let mut other_rank = 0usize;
        for block in 0..SHORT_BLOCKS {
            let (start, width, rank) = if indicators[block] {
                let rank = other_rank;
                other_rank += 1;
                (transient_blocks * SHORT_LINES, other_blocks, rank)
            } else {
                let rank = transient_rank;
                transient_rank += 1;
                (0, transient_blocks, rank)
            };
            for line in 0..SHORT_LINES {
                grouped[start + rank + width * line] = (block * 1000 + line) as f32;
            }
        }

        let mut workspace = SpectrumDegroupWorkspace::new();
        inverse_group_spectrum(TransformType::Short, group, &mut grouped, &mut workspace).unwrap();
        for block in 0..SHORT_BLOCKS {
            for line in 0..SHORT_LINES {
                assert_eq!(
                    grouped[block + SHORT_BLOCKS * line],
                    (block * 1000 + line) as f32
                );
            }
        }
    }

    #[test]
    fn malformed_two_group_vector_is_rejected() {
        let group = GroupSideInfo {
            num_groups: 2,
            group_indicator: [false; SHORT_BLOCKS],
            next_bit_offset: 0,
        };
        let mut spectrum = [0.0_f32; MDCT_LINES];
        let mut workspace = SpectrumDegroupWorkspace::new();
        assert!(
            inverse_group_spectrum(TransformType::Short, group, &mut spectrum, &mut workspace,)
                .is_err()
        );
    }
}
