use yinqidao_codec_core::CodecError;

use crate::{
    BweConfig, BweMode, GaChannelSideInfo, GroupSideInfo, NeuralNetworkType,
    parse_core_side_prefix_at, parse_group_bits_at, parse_qc_side_info_at, qc_fixed_header_bits,
};
use crate::bitreader::BitReader;

pub const MAX_HOA_GROUPS: usize = 3;
pub const MAX_HOA_BASIS: usize = 4;
pub const MAX_HOA_GROUP_PAIRS: usize = 8;
pub const HOA_SCALE_FACTOR_BANDS: usize = 21;
pub const HOA_BASIS_TABLE_LEN: usize = 1_343;
pub const HOA_NO_ILD_INDEX: u8 = 30;

pub const HOA_SFB_BOUNDARIES: [usize; HOA_SCALE_FACTOR_BANDS + 1] = [
    0, 8, 24, 40, 56, 72, 88, 104, 128, 160, 192, 224, 256, 288, 336, 384, 432, 480,
    544, 608, 672, 768,
];

const HOA_RATIO_RANGE: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HoaGroupConfig {
    pub channels: u8,
    pub channel_offset: u8,
    pub pair_index_bits: u8,
    pub core_lines: u16,
    pub bwe_enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HoaConfig {
    pub order: u8,
    pub output_channels: u8,
    pub transport_channels: u8,
    pub foreground_channels: u8,
    pub residual_channels: u8,
    pub default_spatial_analysis: bool,
    pub groups: Vec<HoaGroupConfig>,
}

impl HoaConfig {
    pub fn for_order_bitrate(order: u8, total_bitrate_kbps: u32) -> Result<Self, CodecError> {
        let (default_spatial_analysis, group_channels, core_lines, group_bwe):
            (bool, &[u8], &[u16], &[bool]) = match (order, total_bitrate_kbps) {
            (1, 48 | 96 | 128 | 192 | 256) => (false, &[4], &[1_024], &[true]),
            (2, 192) => (false, &[9], &[352], &[true]),
            (2, 256) => (false, &[9], &[384], &[true]),
            (2, 320) => (false, &[9], &[544], &[true]),
            (2, 384 | 480) => (false, &[9], &[672], &[true]),
            (2, 512) => (false, &[9], &[768], &[false]),
            (2, 640) => (false, &[9], &[800], &[false]),
            (3, 256) => (true, &[2, 6], &[732, 384], &[false, true]),
            (3, 320) => (true, &[2, 7], &[732, 384], &[false, true]),
            (3, 384) => (true, &[2, 9], &[768, 384], &[false, true]),
            (3, 512) => (true, &[2, 10], &[768, 544], &[false, true]),
            (3, 640) => (true, &[2, 12], &[768, 672], &[false, true]),
            (3, 896) => (false, &[16], &[672], &[true]),
            _ => {
                return Err(CodecError::Unsupported(
                    "unsupported AVS3 HOA order/bitrate configuration",
                ));
            }
        };

        let output_channels_u16 = u16::from(order + 1) * u16::from(order + 1);
        let output_channels = u8::try_from(output_channels_u16)
            .map_err(|_| CodecError::InvalidData("HOA output channel count overflow"))?;
        let mut groups = Vec::with_capacity(group_channels.len());
        let mut offset = 0_u8;
        for index in 0..group_channels.len() {
            let channels = group_channels[index];
            groups.push(HoaGroupConfig {
                channels,
                channel_offset: offset,
                pair_index_bits: hoa_pair_index_bits(channels)?,
                core_lines: core_lines[index],
                bwe_enabled: group_bwe[index],
            });
            offset = offset
                .checked_add(channels)
                .ok_or(CodecError::InvalidData("HOA transport channel count overflow"))?;
        }
        let foreground_channels = if order == 1 { 0 } else { group_channels[0] };
        let residual_channels = if order == 1 {
            0
        } else {
            offset.checked_sub(foreground_channels).ok_or(CodecError::Internal(
                "HOA residual channel accounting underflow".into(),
            ))?
        };

        Ok(Self {
            order,
            output_channels,
            transport_channels: offset,
            foreground_channels,
            residual_channels,
            default_spatial_analysis,
            groups,
        })
    }

    pub fn group_for_channel(&self, channel: usize) -> Result<HoaGroupConfig, CodecError> {
        if channel >= usize::from(self.transport_channels) {
            return Err(CodecError::InvalidData(
                "HOA transport channel index exceeds configured channel count",
            ));
        }
        self.groups
            .iter()
            .copied()
            .find(|group| {
                let start = usize::from(group.channel_offset);
                channel >= start && channel < start + usize::from(group.channels)
            })
            .ok_or(CodecError::Internal(
                "HOA transport channel is not covered by a group".into(),
            ))
    }

    pub fn low_bitrate_lsf_precision(&self, total_bitrate_kbps: u32) -> bool {
        total_bitrate_kbps <= u32::from(self.transport_channels).saturating_mul(32)
    }

    pub fn bwe_config_for_channel(
        &self,
        channel: usize,
        total_bitrate_kbps: u32,
    ) -> Result<Option<BweConfig>, CodecError> {
        let group = self.group_for_channel(channel)?;
        if !group.bwe_enabled {
            return Ok(None);
        }
        BweConfig::for_bitrate(
            BweMode::Hoa { order: self.order },
            total_bitrate_kbps,
        )?
        .ok_or(CodecError::Internal(
            "HOA group enables BWE but no HOA BWE configuration was selected".into(),
        ))
        .map(Some)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HoaDmxMode {
    FullBand,
    ScaleFactorBand,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HoaPairSideInfo {
    pub pair_index: u16,
    pub first: u8,
    pub second: u8,
    pub mode: HoaDmxMode,
    pub sfb_mask: [bool; HOA_SCALE_FACTOR_BANDS],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HoaGroupSideInfo {
    pub channels: u8,
    pub channel_offset: u8,
    pub pairs: Vec<HoaPairSideInfo>,
    /// `None` is the transmitted/implicit `MC_ILD_CBLEN` sentinel (30): no gain adjustment.
    pub ild_indices: Vec<Option<u8>>,
    pub group_bits_ratio: u8,
    pub channel_bits_ratio: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HoaSideInfo {
    pub scene_type: u8,
    pub spatial_analysis: bool,
    pub vector_channels: u8,
    pub basis_indices: Vec<u16>,
    pub groups: Vec<HoaGroupSideInfo>,
    pub next_bit_offset: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HoaBitAllocation {
    pub channel_bytes: Vec<usize>,
    pub trailing_bits: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GaHoaFrameSideInfo {
    pub config: HoaConfig,
    pub channels: Vec<GaChannelSideInfo>,
    pub channel_bwe_configs: Vec<Option<BweConfig>>,
    pub hoa: HoaSideInfo,
    pub allocation: HoaBitAllocation,
    pub next_bit_offset: usize,
    pub trailing_bits: usize,
}

pub fn hoa_pair_index_bits(channels: u8) -> Result<u8, CodecError> {
    if channels < 2 {
        return Err(CodecError::InvalidData(
            "HOA coupling group requires at least two channels",
        ));
    }
    let count = u32::from(channels);
    let combinations = count.saturating_mul(count - 1) / 2;
    let bits = u32::BITS - (combinations - 1).leading_zeros();
    u8::try_from(bits.max(1))
        .map_err(|_| CodecError::InvalidData("HOA pair index width overflow"))
}

/// HOA uses the reference decoder's `(0,1),(0,2),(1,2),(0,3),...` pair enumeration.
pub fn resolve_hoa_pair_index(channels: u8, pair_index: u16) -> Result<(u8, u8), CodecError> {
    if channels < 2 {
        return Err(CodecError::InvalidData(
            "HOA pair resolution requires at least two channels",
        ));
    }
    let total_pairs = u32::from(channels).saturating_mul(u32::from(channels - 1)) / 2;
    if u32::from(pair_index) >= total_pairs {
        return Err(CodecError::InvalidData(
            "HOA pair index exceeds available channel combinations",
        ));
    }

    let mut current = 0_u16;
    for second in 1..channels {
        for first in 0..second {
            if current == pair_index {
                return Ok((first, second));
            }
            current += 1;
        }
    }
    Err(CodecError::Internal(
        "validated HOA pair index escaped channel-pair enumeration".into(),
    ))
}

pub fn parse_hoa_side_info_at(
    payload: &[u8],
    bit_offset: usize,
    config: &HoaConfig,
) -> Result<HoaSideInfo, CodecError> {
    let mut reader = BitReader::with_bit_position(payload, bit_offset)?;
    let scene_type = reader.read_bits(4)? as u8;
    let spatial_analysis = reader.read_bit()?;
    let mut vector_channels = if config.default_spatial_analysis {
        config.foreground_channels
    } else {
        0
    };
    if spatial_analysis {
        vector_channels = reader.read_bits(4)? as u8;
        let limit = usize::from(config.transport_channels)
            .checked_sub(usize::from(config.residual_channels))
            .ok_or(CodecError::InvalidData("HOA recovery layout underflow"))?
            .min(MAX_HOA_BASIS);
        if vector_channels == 0 || usize::from(vector_channels) > limit {
            return Err(CodecError::InvalidData(
                "HOA vector-channel count exceeds the configured recovery basis",
            ));
        }
    }
    if usize::from(vector_channels) > MAX_HOA_BASIS {
        return Err(CodecError::InvalidData(
            "HOA vector-channel count exceeds four basis vectors",
        ));
    }

    let mut basis_indices = Vec::with_capacity(usize::from(vector_channels));
    for _ in 0..vector_channels {
        let index = reader.read_bits(12)? as u16;
        if usize::from(index) >= HOA_BASIS_TABLE_LEN {
            return Err(CodecError::InvalidData(
                "HOA basis index exceeds the normative fixed-angle table",
            ));
        }
        basis_indices.push(index);
    }

    let mut groups = Vec::with_capacity(config.groups.len());
    for group in &config.groups {
        let pair_count = reader.read_bits(4)? as usize;
        if pair_count > MAX_HOA_GROUP_PAIRS {
            return Err(CodecError::InvalidData(
                "HOA group pair count exceeds decoder capacity",
            ));
        }
        let mut pairs = Vec::with_capacity(pair_count);
        for _ in 0..pair_count {
            let pair_index = reader.read_bits(group.pair_index_bits)? as u16;
            let (first, second) = resolve_hoa_pair_index(group.channels, pair_index)?;
            let mode = if reader.read_bit()? {
                HoaDmxMode::ScaleFactorBand
            } else {
                HoaDmxMode::FullBand
            };
            let mut sfb_mask = [true; HOA_SCALE_FACTOR_BANDS];
            if mode == HoaDmxMode::ScaleFactorBand {
                for enabled in &mut sfb_mask {
                    *enabled = reader.read_bit()?;
                }
            }
            pairs.push(HoaPairSideInfo {
                pair_index,
                first,
                second,
                mode,
                sfb_mask,
            });
        }

        let mut ild_indices = vec![None; usize::from(group.channels)];
        if pair_count != 0 {
            for ild in &mut ild_indices {
                let index = reader.read_bits(5)? as u8;
                *ild = match index {
                    0..=29 => Some(index),
                    HOA_NO_ILD_INDEX => None,
                    _ => {
                        return Err(CodecError::InvalidData(
                            "HOA ILD index 31 is reserved",
                        ));
                    }
                };
            }
        }

        let group_bits_ratio = reader.read_bits(4)? as u8;
        let mut channel_bits_ratio = Vec::with_capacity(usize::from(group.channels));
        for _ in 0..group.channels {
            channel_bits_ratio.push(reader.read_bits(4)? as u8);
        }
        groups.push(HoaGroupSideInfo {
            channels: group.channels,
            channel_offset: group.channel_offset,
            pairs,
            ild_indices,
            group_bits_ratio,
            channel_bits_ratio,
        });
    }

    Ok(HoaSideInfo {
        scene_type,
        spatial_analysis,
        vector_channels,
        basis_indices,
        groups,
        next_bit_offset: reader.position_bits(),
    })
}

pub fn allocate_hoa_bytes(
    remaining_payload_bits: usize,
    nn_type: NeuralNetworkType,
    groups: &[GroupSideInfo],
    config: &HoaConfig,
    side: &HoaSideInfo,
) -> Result<HoaBitAllocation, CodecError> {
    let channels = usize::from(config.transport_channels);
    if groups.len() != channels || side.groups.len() != config.groups.len() {
        return Err(CodecError::InvalidData(
            "HOA side information does not match transport geometry",
        ));
    }

    let fixed_qc_bits = groups.iter().try_fold(0_usize, |sum, group| {
        sum.checked_add(qc_fixed_header_bits(nn_type, group.num_groups)?)
            .ok_or(CodecError::InvalidData("HOA QC header size overflow"))
    })?;
    let available_bits = remaining_payload_bits
        .checked_sub(fixed_qc_bits)
        .ok_or(CodecError::Truncated)?;
    let total_bytes = available_bits / 8;
    let trailing_bits = available_bits & 7;

    let mut group_bytes = vec![0_usize; config.groups.len()];
    let mut remaining_bytes = total_bytes;
    if config.groups.len() > 1 {
        for group_index in 0..config.groups.len() - 1 {
            let requested = total_bytes
                .checked_mul(usize::from(side.groups[group_index].group_bits_ratio))
                .ok_or(CodecError::InvalidData("HOA group byte allocation overflow"))?
                / HOA_RATIO_RANGE;
            if requested > remaining_bytes {
                return Err(CodecError::InvalidData(
                    "HOA group bit ratio exceeds the remaining payload budget",
                ));
            }
            group_bytes[group_index] = requested;
            remaining_bytes -= requested;
        }
    }
    if let Some(last) = group_bytes.last_mut() {
        *last = remaining_bytes;
    }

    let mut channel_bytes = vec![0_usize; channels];
    for (group_index, group_config) in config.groups.iter().enumerate() {
        let group_side = &side.groups[group_index];
        if group_side.channels != group_config.channels
            || group_side.channel_offset != group_config.channel_offset
            || group_side.channel_bits_ratio.len() != usize::from(group_config.channels)
        {
            return Err(CodecError::InvalidData(
                "HOA group side information does not match bitrate configuration",
            ));
        }
        let ratio_sum = group_side.channel_bits_ratio.iter().try_fold(
            0_usize,
            |sum, ratio| {
                sum.checked_add(usize::from(*ratio))
                    .ok_or(CodecError::InvalidData("HOA channel ratio sum overflow"))
            },
        )?;
        if ratio_sum == 0 {
            return Err(CodecError::InvalidData(
                "HOA group channel bit ratios sum to zero",
            ));
        }
        let unit = group_bytes[group_index] / ratio_sum;
        let residual = group_bytes[group_index] % ratio_sum;
        let start = usize::from(group_config.channel_offset);
        let end = start + usize::from(group_config.channels);
        let output = &mut channel_bytes[start..end];
        for (destination, ratio) in output.iter_mut().zip(&group_side.channel_bits_ratio) {
            *destination = unit
                .checked_mul(usize::from(*ratio))
                .ok_or(CodecError::InvalidData("HOA channel byte allocation overflow"))?;
        }

        if residual >= output.len() {
            let increment = residual / output.len();
            for destination in output.iter_mut() {
                *destination = destination
                    .checked_add(increment)
                    .ok_or(CodecError::InvalidData("HOA residual byte allocation overflow"))?;
            }
            output[0] = output[0]
                .checked_add(residual % output.len())
                .ok_or(CodecError::InvalidData("HOA residual byte allocation overflow"))?;
        } else {
            output[0] = output[0]
                .checked_add(residual)
                .ok_or(CodecError::InvalidData("HOA residual byte allocation overflow"))?;
        }
    }

    if channel_bytes.iter().sum::<usize>() != total_bytes {
        return Err(CodecError::Internal(
            "HOA byte allocation does not conserve entropy payload bytes".into(),
        ));
    }
    Ok(HoaBitAllocation {
        channel_bytes,
        trailing_bits,
    })
}

pub fn parse_hoa_frame_side_info(
    payload: &[u8],
    core_bit_offset: usize,
    order: u8,
    total_bitrate_kbps: u32,
    nn_type: NeuralNetworkType,
) -> Result<GaHoaFrameSideInfo, CodecError> {
    if matches!(nn_type, NeuralNetworkType::Reserved(_)) {
        return Err(CodecError::Unsupported(
            "reserved AVS3 neural-network type",
        ));
    }
    let config = HoaConfig::for_order_bitrate(order, total_bitrate_kbps)?;
    let low_bitrate_precision = config.low_bitrate_lsf_precision(total_bitrate_kbps);
    let channel_count = usize::from(config.transport_channels);

    let mut offset = core_bit_offset;
    let mut cores = Vec::with_capacity(channel_count);
    let mut bwe_sides = Vec::with_capacity(channel_count);
    let mut channel_bwe_configs = Vec::with_capacity(channel_count);
    for channel in 0..channel_count {
        let core = parse_core_side_prefix_at(payload, offset, low_bitrate_precision)?;
        offset = core.next_bit_offset;
        let bwe_config = config.bwe_config_for_channel(channel, total_bitrate_kbps)?;
        let bwe = if let Some(bwe_config) = bwe_config {
            let side = bwe_config.parse_side_info(payload, offset)?;
            offset = side.next_bit_offset;
            Some(side)
        } else {
            None
        };
        cores.push(core);
        bwe_sides.push(bwe);
        channel_bwe_configs.push(bwe_config);
    }

    let mut groups = Vec::with_capacity(channel_count);
    for core in &cores {
        let group = parse_group_bits_at(payload, offset, core.transform_type)?;
        offset = group.next_bit_offset;
        groups.push(group);
    }

    let hoa = parse_hoa_side_info_at(payload, offset, &config)?;
    offset = hoa.next_bit_offset;
    let payload_bits = payload.len().saturating_mul(8);
    let remaining_payload_bits = payload_bits
        .checked_sub(offset)
        .ok_or(CodecError::Truncated)?;
    let allocation = allocate_hoa_bytes(
        remaining_payload_bits,
        nn_type,
        &groups,
        &config,
        &hoa,
    )?;

    let mut qcs = Vec::with_capacity(channel_count);
    for (group, &channel_bytes) in groups.iter().zip(&allocation.channel_bytes) {
        let qc = parse_qc_side_info_at(
            payload,
            offset,
            nn_type,
            group.num_groups,
            channel_bytes,
        )?;
        offset = qc.next_bit_offset;
        qcs.push(qc);
    }
    let actual_tail = payload_bits.checked_sub(offset).ok_or(CodecError::Truncated)?;
    if actual_tail != allocation.trailing_bits {
        return Err(CodecError::Internal(
            "HOA QC parsing does not match payload byte allocation".into(),
        ));
    }

    let channels = cores
        .into_iter()
        .zip(bwe_sides)
        .zip(groups)
        .zip(qcs)
        .map(|(((core, bwe), group), qc)| GaChannelSideInfo {
            core,
            bwe,
            group,
            qc,
        })
        .collect();

    Ok(GaHoaFrameSideInfo {
        config,
        channels,
        channel_bwe_configs,
        hoa,
        trailing_bits: allocation.trailing_bits,
        allocation,
        next_bit_offset: offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hoa_transport_configuration_matches_normative_order_three_rows() {
        let low = HoaConfig::for_order_bitrate(3, 256).unwrap();
        assert_eq!(low.transport_channels, 8);
        assert_eq!(low.output_channels, 16);
        assert_eq!(low.groups[0].channels, 2);
        assert_eq!(low.groups[1].channels, 6);
        assert!(low.default_spatial_analysis);
        assert!(!low.groups[0].bwe_enabled);
        assert!(low.groups[1].bwe_enabled);

        let high = HoaConfig::for_order_bitrate(3, 896).unwrap();
        assert_eq!(high.transport_channels, 16);
        assert_eq!(high.groups.len(), 1);
        assert!(!high.default_spatial_analysis);
    }

    #[test]
    fn hoa_pair_order_follows_reference_column_walk() {
        assert_eq!(resolve_hoa_pair_index(4, 0).unwrap(), (0, 1));
        assert_eq!(resolve_hoa_pair_index(4, 1).unwrap(), (0, 2));
        assert_eq!(resolve_hoa_pair_index(4, 2).unwrap(), (1, 2));
        assert_eq!(resolve_hoa_pair_index(4, 3).unwrap(), (0, 3));
        assert_eq!(resolve_hoa_pair_index(4, 5).unwrap(), (2, 3));
        assert!(resolve_hoa_pair_index(4, 6).is_err());
    }

    #[test]
    fn hoa_two_group_allocation_conserves_all_whole_entropy_bytes() {
        let config = HoaConfig::for_order_bitrate(3, 256).unwrap();
        let groups = vec![
            GroupSideInfo {
                num_groups: 1,
                group_indicator: [false; 8],
                next_bit_offset: 0,
            };
            8
        ];
        let side = HoaSideInfo {
            scene_type: 0,
            spatial_analysis: false,
            vector_channels: 2,
            basis_indices: vec![0, 1],
            groups: vec![
                HoaGroupSideInfo {
                    channels: 2,
                    channel_offset: 0,
                    pairs: Vec::new(),
                    ild_indices: vec![None; 2],
                    group_bits_ratio: 4,
                    channel_bits_ratio: vec![1, 1],
                },
                HoaGroupSideInfo {
                    channels: 6,
                    channel_offset: 2,
                    pairs: Vec::new(),
                    ild_indices: vec![None; 6],
                    group_bits_ratio: 0,
                    channel_bits_ratio: vec![1; 6],
                },
            ],
            next_bit_offset: 0,
        };
        let fixed = groups
            .iter()
            .map(|group| qc_fixed_header_bits(NeuralNetworkType::Basic, group.num_groups).unwrap())
            .sum::<usize>();
        let allocation = allocate_hoa_bytes(
            fixed + 100 * 8 + 3,
            NeuralNetworkType::Basic,
            &groups,
            &config,
            &side,
        )
        .unwrap();
        assert_eq!(allocation.channel_bytes.iter().sum::<usize>(), 100);
        assert_eq!(allocation.trailing_bits, 3);
        assert_eq!(allocation.channel_bytes[0] + allocation.channel_bytes[1], 25);
    }
}
