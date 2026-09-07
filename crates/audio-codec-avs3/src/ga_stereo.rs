use yinqidao_codec_core::CodecError;

use crate::{
    BweConfig, BweSideInfo, GaChannelSideInfo, GroupSideInfo, NeuralNetworkType, QcSideInfo,
    TransformType, bitreader::BitReader, parse_core_side_prefix_at, parse_group_bits_at,
    parse_qc_side_info_at, qc_fixed_header_bits,
};

const STEREO_CHANNELS: usize = 2;
const MCR_VQ_VECTORS: usize = 6;
const STEREO_RATIO_GROUPS: usize = 1 << 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StereoCouplingSideInfo {
    /// Conventional two-downmix-channel allocation, optionally followed by M/S inverse upmix.
    Ms {
        is_ms: bool,
        ild_q_idx: Option<u8>,
        bits_ratio: u8,
    },
    /// MCR side information. The current decoder parses these indices exactly but deliberately does
    /// not consume them for upmix until the referenced GB/T 33475.3-2018 B.154/B.155 codebooks are
    /// installed.
    Mcr {
        is_short_window: bool,
        vq_bits: u8,
        even_vq_indices: [u16; MCR_VQ_VECTORS],
        odd_vq_indices: [u16; MCR_VQ_VECTORS],
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StereoSideInfo {
    pub coupling: StereoCouplingSideInfo,
    pub next_bit_offset: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GaStereoFrameSideInfo {
    pub channels: [GaChannelSideInfo; STEREO_CHANNELS],
    pub bwe_config: Option<BweConfig>,
    pub stereo: StereoSideInfo,
    pub channel_bytes: [usize; STEREO_CHANNELS],
    pub next_bit_offset: usize,
    /// Unused storage bits after the byte-counted QC payloads.
    pub trailing_bits: usize,
}

/// Parse table-18 stereo coupling syntax at a known frame bit offset.
///
/// `useMcr` is derived by the standard from the total dual-channel stereo bitrate, not transmitted:
/// <=32 kb/s selects MCR; >32 kb/s selects the M/S-capable syntax.
pub fn parse_stereo_side_info_at(
    bytes: &[u8],
    bit_offset: usize,
    total_bitrate_kbps: u32,
    left_transform: TransformType,
) -> Result<StereoSideInfo, CodecError> {
    let mut reader = BitReader::with_bit_position(bytes, bit_offset)?;
    let coupling = if total_bitrate_kbps <= 32 {
        let is_short_window = left_transform == TransformType::Short;
        let vq_bits = if is_short_window { 8 } else { 9 };
        let mut even_vq_indices = [0_u16; MCR_VQ_VECTORS];
        let mut odd_vq_indices = [0_u16; MCR_VQ_VECTORS];
        for index in 0..MCR_VQ_VECTORS {
            even_vq_indices[index] = reader.read_bits(vq_bits)? as u16;
            odd_vq_indices[index] = reader.read_bits(vq_bits)? as u16;
        }
        StereoCouplingSideInfo::Mcr {
            is_short_window,
            vq_bits,
            even_vq_indices,
            odd_vq_indices,
        }
    } else {
        let is_ms = reader.read_bit()?;
        let ild_q_idx = if is_ms {
            let index = reader.read_bits(4)? as u8;
            if index == 0 {
                return Err(CodecError::InvalidData(
                    "stereo ILD index zero would make inverse ILD undefined",
                ));
            }
            Some(index)
        } else {
            None
        };
        let bits_ratio = reader.read_bits(3)? as u8;
        StereoCouplingSideInfo::Ms {
            is_ms,
            ild_q_idx,
            bits_ratio,
        }
    };

    Ok(StereoSideInfo {
        coupling,
        next_bit_offset: reader.position_bits(),
    })
}

/// Normative M/S-mode byte split after core/group/stereo side bits have already been consumed.
///
/// `remaining_payload_bits` starts immediately after table-18 side information. Fixed table-14 QC
/// fields are removed per channel, the remainder is rounded down to whole bytes, and the 3-bit
/// `bitsRatio` allocates an integer number of eighths to downmixed channel 0. Any byte remainder
/// from the eighth-group division belongs to channel 1 exactly as formulas (5)..(7) specify.
pub fn allocate_stereo_ms_bytes(
    remaining_payload_bits: usize,
    nn_type: NeuralNetworkType,
    groups: [GroupSideInfo; STEREO_CHANNELS],
    bits_ratio: u8,
) -> Result<([usize; STEREO_CHANNELS], usize), CodecError> {
    if bits_ratio >= STEREO_RATIO_GROUPS as u8 {
        return Err(CodecError::InvalidData("stereo bitsRatio exceeds its 3-bit domain"));
    }
    let fixed_qc_bits = qc_fixed_header_bits(nn_type, groups[0].num_groups)?
        .checked_add(qc_fixed_header_bits(nn_type, groups[1].num_groups)?)
        .ok_or(CodecError::InvalidData("stereo QC header size overflow"))?;
    let available_bits = remaining_payload_bits
        .checked_sub(fixed_qc_bits)
        .ok_or(CodecError::Truncated)?;
    let available_bytes = available_bits / 8;
    let bytes_per_ratio_group = available_bytes / STEREO_RATIO_GROUPS;
    let channel_zero = usize::from(bits_ratio)
        .checked_mul(bytes_per_ratio_group)
        .ok_or(CodecError::InvalidData("stereo byte allocation overflow"))?;
    let channel_one = available_bytes
        .checked_sub(channel_zero)
        .ok_or(CodecError::InvalidData("stereo byte allocation underflow"))?;
    Ok(([channel_zero, channel_one], available_bits & 7))
}

fn parse_channel_core_and_bwe(
    payload: &[u8],
    bit_offset: usize,
    low_bitrate_precision: bool,
    bwe_config: Option<BweConfig>,
) -> Result<(crate::CoreSidePrefix, Option<BweSideInfo>, usize), CodecError> {
    let core = parse_core_side_prefix_at(payload, bit_offset, low_bitrate_precision)?;
    let mut next = core.next_bit_offset;
    let bwe = if let Some(config) = bwe_config {
        let side = config.parse_side_info(payload, next)?;
        next = side.next_bit_offset;
        Some(side)
    } else {
        None
    };
    Ok((core, bwe, next))
}

/// Parse a complete general-full-rate stereo frame through both byte-counted QC payloads.
///
/// The normative frame-major order is preserved: both core/BWE blocks, both grouping blocks,
/// one stereo side block, stereo byte allocation, then two `DecodeQcBits()` blocks. The current
/// executable path supports the >32-kb/s M/S mode; <=32-kb/s MCR syntax is parsed by
/// `parse_stereo_side_info_at` but its single-channel QC allocation/upmix remains a separate
/// milestone because it depends on GB/T 33475.3-2018 B.154/B.155.
pub fn parse_stereo_frame_side_info(
    payload: &[u8],
    core_bit_offset: usize,
    nn_type: NeuralNetworkType,
    low_bitrate_precision: bool,
    bwe_config: Option<BweConfig>,
    total_bitrate_kbps: u32,
) -> Result<GaStereoFrameSideInfo, CodecError> {
    let (core0, bwe0, after_core0) = parse_channel_core_and_bwe(
        payload,
        core_bit_offset,
        low_bitrate_precision,
        bwe_config,
    )?;
    let (core1, bwe1, after_core1) = parse_channel_core_and_bwe(
        payload,
        after_core0,
        low_bitrate_precision,
        bwe_config,
    )?;

    let group0 = parse_group_bits_at(payload, after_core1, core0.transform_type)?;
    let group1 = parse_group_bits_at(payload, group0.next_bit_offset, core1.transform_type)?;
    let stereo = parse_stereo_side_info_at(
        payload,
        group1.next_bit_offset,
        total_bitrate_kbps,
        core0.transform_type,
    )?;

    let StereoCouplingSideInfo::Ms { bits_ratio, .. } = stereo.coupling else {
        return Err(CodecError::Unsupported(
            "AVS3 MCR stereo QC allocation/upmix requires GB/T 33475.3 B.154/B.155",
        ));
    };

    let payload_bits = payload.len().saturating_mul(8);
    let remaining_payload_bits = payload_bits
        .checked_sub(stereo.next_bit_offset)
        .ok_or(CodecError::Truncated)?;
    let groups = [group0, group1];
    let (channel_bytes, trailing_bits) =
        allocate_stereo_ms_bytes(remaining_payload_bits, nn_type, groups, bits_ratio)?;

    let qc0 = parse_qc_side_info_at(
        payload,
        stereo.next_bit_offset,
        nn_type,
        group0.num_groups,
        channel_bytes[0],
    )?;
    let qc1 = parse_qc_side_info_at(
        payload,
        qc0.next_bit_offset,
        nn_type,
        group1.num_groups,
        channel_bytes[1],
    )?;
    let actual_tail = payload_bits
        .checked_sub(qc1.next_bit_offset)
        .ok_or(CodecError::Truncated)?;
    if actual_tail != trailing_bits {
        return Err(CodecError::Internal(
            "stereo QC parsing does not match StereoBitsAllocation payload accounting".into(),
        ));
    }

    Ok(GaStereoFrameSideInfo {
        channels: [
            GaChannelSideInfo {
                core: core0,
                bwe: bwe0,
                group: group0,
                qc: qc0,
            },
            GaChannelSideInfo {
                core: core1,
                bwe: bwe1,
                group: group1,
                qc: qc1,
            },
        ],
        bwe_config,
        stereo,
        channel_bytes,
        next_bit_offset: qc1.next_bit_offset,
        trailing_bits,
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
            Self { bytes: Vec::new(), bit_pos: 0 }
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
    fn parses_non_mcr_ms_side_fields_exactly() {
        let mut writer = BitWriter::new();
        writer.push(1, 1);
        writer.push(9, 4);
        writer.push(5, 3);
        let side = parse_stereo_side_info_at(&writer.bytes, 0, 64, TransformType::Long).unwrap();
        assert_eq!(
            side.coupling,
            StereoCouplingSideInfo::Ms {
                is_ms: true,
                ild_q_idx: Some(9),
                bits_ratio: 5,
            }
        );
        assert_eq!(side.next_bit_offset, 8);
    }

    #[test]
    fn mcr_side_uses_six_vector_pairs_and_window_dependent_width() {
        let mut short = BitWriter::new();
        for index in 0..MCR_VQ_VECTORS {
            short.push(index as u32, 8);
            short.push((index + 16) as u32, 8);
        }
        let info = parse_stereo_side_info_at(&short.bytes, 0, 32, TransformType::Short).unwrap();
        let StereoCouplingSideInfo::Mcr {
            vq_bits,
            even_vq_indices,
            odd_vq_indices,
            ..
        } = info.coupling
        else {
            panic!("expected MCR side info");
        };
        assert_eq!(vq_bits, 8);
        assert_eq!(even_vq_indices, [0, 1, 2, 3, 4, 5]);
        assert_eq!(odd_vq_indices, [16, 17, 18, 19, 20, 21]);
        assert_eq!(info.next_bit_offset, 96);
    }

    #[test]
    fn stereo_ms_allocation_preserves_eighth_group_rounding() {
        let group = GroupSideInfo {
            num_groups: 1,
            group_indicator: [false; 8],
            next_bit_offset: 0,
        };
        let (bytes, tail) = allocate_stereo_ms_bytes(
            38 + 16 * 8 + 6,
            NeuralNetworkType::Basic,
            [group, group],
            3,
        )
        .unwrap();
        assert_eq!(bytes, [6, 10]);
        assert_eq!(tail, 6);
    }

    #[test]
    fn parses_complete_basic_ms_frame_and_qc_ranges() {
        let mut writer = BitWriter::new();
        writer.zeros(50); // ch0 long core, high-precision FD, two disabled TNS filters
        writer.zeros(50); // ch1
        writer.push(1, 1); // isMs
        writer.push(8, 4); // IldQIdx
        writer.push(3, 3); // bitsRatio -> 3/8 of byte groups to ch0

        // channel 0: 19 fixed Basic bits + six range-coded bytes
        writer.push(0, 1);
        writer.push(127, 7);
        writer.push(0, 3);
        writer.push(0, 8);
        writer.zeros(6 * 8);

        // channel 1: 19 fixed Basic bits + ten range-coded bytes
        writer.push(0, 1);
        writer.push(127, 7);
        writer.push(0, 3);
        writer.push(0, 8);
        writer.zeros(10 * 8);

        let frame = parse_stereo_frame_side_info(
            &writer.bytes,
            0,
            NeuralNetworkType::Basic,
            false,
            None,
            64,
        )
        .unwrap();
        assert_eq!(frame.channel_bytes, [6, 10]);
        assert_eq!(frame.channels[0].qc.channel_bytes, 6);
        assert_eq!(frame.channels[1].qc.channel_bytes, 10);
        assert_eq!(frame.next_bit_offset, writer.bit_pos);
        assert_eq!(frame.trailing_bits, writer.bytes.len() * 8 - writer.bit_pos);
        assert!(frame.trailing_bits < 8);
    }

    #[test]
    fn complete_mcr_frame_stops_before_guessing_qc_layout() {
        let mut writer = BitWriter::new();
        writer.zeros(50);
        writer.zeros(50);
        for _ in 0..MCR_VQ_VECTORS * 2 {
            writer.push(0, 9);
        }
        assert!(matches!(
            parse_stereo_frame_side_info(
                &writer.bytes,
                0,
                NeuralNetworkType::Basic,
                false,
                None,
                32,
            ),
            Err(CodecError::Unsupported(_))
        ));
    }
}
