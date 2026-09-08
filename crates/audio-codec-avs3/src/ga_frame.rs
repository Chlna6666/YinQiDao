use yinqidao_codec_core::CodecError;

use crate::{
    BweConfig, BweSideInfo, CoreSidePrefix, GroupSideInfo, McBitAllocation, MultichannelSideInfo,
    NeuralNetworkType, QcSideInfo, allocate_multichannel_bytes, parse_core_side_prefix_at,
    parse_group_bits_at, parse_multichannel_side_info_at, parse_qc_side_info_at,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GaChannelSideInfo {
    pub core: CoreSidePrefix,
    pub bwe: Option<BweSideInfo>,
    pub group: GroupSideInfo,
    pub qc: QcSideInfo,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GaMultichannelFrameSideInfo {
    pub channels: Vec<GaChannelSideInfo>,
    pub bwe_config: Option<BweConfig>,
    pub multichannel: MultichannelSideInfo,
    pub allocation: McBitAllocation,
    pub next_bit_offset: usize,
    pub trailing_bits: usize,
}

/// Parse table-10 multichannel syntax through all `DecodeQcBits()` payload ranges.
///
/// Ordering is frame-major exactly as specified: all core side blocks (including per-channel BWE),
/// all grouping blocks, one multichannel side block, bit allocation, then one QC block per channel.
pub fn parse_multichannel_frame_side_info(
    payload: &[u8],
    core_bit_offset: usize,
    channel_count: u16,
    nn_type: NeuralNetworkType,
    low_bitrate_precision: bool,
    bwe_config: Option<BweConfig>,
    total_bitrate_kbps: u32,
    lfe_index: Option<usize>,
) -> Result<GaMultichannelFrameSideInfo, CodecError> {
    if channel_count < 3 {
        return Err(CodecError::InvalidData(
            "multichannel frame parser requires at least three channels",
        ));
    }
    let channel_count_usize = usize::from(channel_count);
    if let Some(index) = lfe_index
        && index >= channel_count_usize
    {
        return Err(CodecError::InvalidData(
            "multichannel LFE index exceeds channel count",
        ));
    }

    let mut offset = core_bit_offset;
    let mut cores = Vec::with_capacity(channel_count_usize);
    let mut bwe = Vec::with_capacity(channel_count_usize);

    for _ in 0..channel_count_usize {
        let core = parse_core_side_prefix_at(payload, offset, low_bitrate_precision)?;
        offset = core.next_bit_offset;
        let side = if let Some(config) = bwe_config {
            let side = config.parse_side_info(payload, offset)?;
            offset = side.next_bit_offset;
            Some(side)
        } else {
            None
        };
        cores.push(core);
        bwe.push(side);
    }

    let mut groups = Vec::with_capacity(channel_count_usize);
    for core in &cores {
        let group = parse_group_bits_at(payload, offset, core.transform_type)?;
        offset = group.next_bit_offset;
        groups.push(group);
    }

    let couple_ch_num = channel_count
        .checked_sub(u16::from(lfe_index.is_some()))
        .ok_or(CodecError::InvalidData(
            "invalid multichannel coupled-channel count",
        ))?;
    let multichannel = parse_multichannel_side_info_at(payload, offset, couple_ch_num)?;
    offset = multichannel.next_bit_offset;

    let payload_bits = payload.len().saturating_mul(8);
    let remaining_payload_bits = payload_bits
        .checked_sub(offset)
        .ok_or(CodecError::Truncated)?;
    let allocation = allocate_multichannel_bytes(
        remaining_payload_bits,
        nn_type,
        &groups,
        &multichannel,
        total_bitrate_kbps,
        lfe_index,
    )?;

    let mut qc = Vec::with_capacity(channel_count_usize);
    for (group, &channel_bytes) in groups.iter().zip(&allocation.channel_bytes) {
        let info =
            parse_qc_side_info_at(payload, offset, nn_type, group.num_groups, channel_bytes)?;
        offset = info.next_bit_offset;
        qc.push(info);
    }

    let actual_tail = payload_bits
        .checked_sub(offset)
        .ok_or(CodecError::Truncated)?;
    if actual_tail != allocation.trailing_bits {
        return Err(CodecError::Internal(
            "multichannel QC parsing does not match McBitsAllocation payload accounting".into(),
        ));
    }

    let channels = cores
        .into_iter()
        .zip(bwe)
        .zip(groups)
        .zip(qc)
        .map(|(((core, bwe), group), qc)| GaChannelSideInfo {
            core,
            bwe,
            group,
            qc,
        })
        .collect();

    Ok(GaMultichannelFrameSideInfo {
        channels,
        bwe_config,
        multichannel,
        trailing_bits: allocation.trailing_bits,
        allocation,
        next_bit_offset: offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BitWriter {
        bytes: Vec<u8>,
        bit_pos: usize,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                bit_pos: 0,
            }
        }

        fn push(&mut self, value: u32, bits: usize) {
            for shift in (0..bits).rev() {
                if self.bit_pos & 7 == 0 {
                    self.bytes.push(0);
                }
                if (value >> shift) & 1 != 0 {
                    let byte = self.bytes.len() - 1;
                    self.bytes[byte] |= 1 << (7 - (self.bit_pos & 7));
                }
                self.bit_pos += 1;
            }
        }

        fn zeros(&mut self, bits: usize) {
            for _ in 0..bits {
                self.push(0, 1);
            }
        }
    }

    #[test]
    fn parses_three_channel_frame_in_normative_frame_major_order() {
        let mut writer = BitWriter::new();
        for _ in 0..3 {
            writer.zeros(50); // long transform + high-precision FD + two disabled TNS filters
        }
        writer.push(0, 1); // HasSilFlag
        writer.push(0, 4); // pairCnt
        writer.push(21, 6);
        writer.push(21, 6);
        writer.push(22, 6);

        for channel_bytes in [9_usize, 9, 12] {
            writer.push(0, 1);
            writer.push(0, 7);
            writer.push(0, 3);
            writer.push(0, 8);
            writer.zeros(channel_bytes * 8);
        }

        let info = parse_multichannel_frame_side_info(
            &writer.bytes,
            0,
            3,
            NeuralNetworkType::Basic,
            false,
            None,
            192,
            None,
        )
        .unwrap();
        assert_eq!(info.channels.len(), 3);
        assert_eq!(info.allocation.channel_bytes, vec![9, 9, 12]);
        assert_eq!(info.channels[0].group.num_groups, 1);
        assert_eq!(info.channels[2].qc.channel_bytes, 12);
        assert_eq!(info.next_bit_offset, writer.bit_pos);
        assert_eq!(info.trailing_bits, 2); // final storage byte contains two padding bits
    }
}
