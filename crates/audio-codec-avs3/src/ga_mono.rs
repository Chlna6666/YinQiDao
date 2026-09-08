use yinqidao_codec_core::CodecError;

use crate::{
    BweConfig, GaChannelSideInfo, NeuralNetworkType, parse_core_side_prefix_at,
    parse_group_bits_at, parse_qc_side_info_at, qc_fixed_header_bits,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GaMonoFrameSideInfo {
    pub channel: GaChannelSideInfo,
    pub bwe_config: Option<BweConfig>,
    pub next_bit_offset: usize,
    /// Unused storage bits after the byte-counted QC payload.
    pub trailing_bits: usize,
}

/// Parse the complete general-full-rate mono syntax through table-14 `DecodeQcBits()`.
///
/// The mono bit allocation is normative and deterministic: after core/BWE/group side information,
/// subtract the fixed QC fields, round the remaining range-coded payload down to whole bytes, and
/// leave at most seven storage bits at the end of the frame. No payload bytes are copied.
pub fn parse_mono_frame_side_info(
    payload: &[u8],
    core_bit_offset: usize,
    nn_type: NeuralNetworkType,
    low_bitrate_precision: bool,
    bwe_config: Option<BweConfig>,
) -> Result<GaMonoFrameSideInfo, CodecError> {
    let core = parse_core_side_prefix_at(payload, core_bit_offset, low_bitrate_precision)?;
    let mut offset = core.next_bit_offset;

    let bwe = if let Some(config) = bwe_config {
        let side = config.parse_side_info(payload, offset)?;
        offset = side.next_bit_offset;
        Some(side)
    } else {
        None
    };

    let group = parse_group_bits_at(payload, offset, core.transform_type)?;
    offset = group.next_bit_offset;

    let payload_bits = payload.len().saturating_mul(8);
    let remaining = payload_bits
        .checked_sub(offset)
        .ok_or(CodecError::Truncated)?;
    let fixed_qc_bits = qc_fixed_header_bits(nn_type, group.num_groups)?;
    let coded_bits = remaining
        .checked_sub(fixed_qc_bits)
        .ok_or(CodecError::Truncated)?;
    let channel_bytes = coded_bits / 8;
    let trailing_bits = coded_bits & 7;

    let qc = parse_qc_side_info_at(payload, offset, nn_type, group.num_groups, channel_bytes)?;
    let actual_tail = payload_bits
        .checked_sub(qc.next_bit_offset)
        .ok_or(CodecError::Truncated)?;
    if actual_tail != trailing_bits {
        return Err(CodecError::Internal(
            "mono QC parsing does not match payload byte accounting".into(),
        ));
    }

    let next_bit_offset = qc.next_bit_offset;
    Ok(GaMonoFrameSideInfo {
        channel: GaChannelSideInfo {
            core,
            bwe,
            group,
            qc,
        },
        bwe_config,
        next_bit_offset,
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
    fn mono_basic_frame_accounts_fixed_qc_bits_and_storage_tail() {
        let mut writer = BitWriter::new();
        writer.zeros(50); // long transform + high-precision FD + two disabled TNS filters

        // Basic-profile QC fixed fields, one group: 1 + 7 + 3 + 8 = 19 bits.
        writer.push(0, 1); // isFeatAmplified
        writer.push(127, 7); // scaleQIdx
        writer.push(0, 3); // nfParamQIdx
        writer.push(0, 8); // contextNumBytes
        writer.zeros(4 * 8); // four base range-coded bytes

        let info =
            parse_mono_frame_side_info(&writer.bytes, 0, NeuralNetworkType::Basic, false, None)
                .unwrap();

        assert_eq!(info.channel.group.num_groups, 1);
        assert_eq!(info.channel.qc.channel_bytes, 4);
        assert_eq!(info.channel.qc.context_bitstream.bit_len, 0);
        assert_eq!(info.channel.qc.base_bitstream.bit_len, 32);
        assert_eq!(info.trailing_bits, writer.bytes.len() * 8 - writer.bit_pos);
        assert!(info.trailing_bits < 8);
    }

    #[test]
    fn mono_parser_rejects_frame_without_room_for_qc_header() {
        let payload = [0_u8; 7]; // 56 bits: only six bits remain after the 50-bit core.
        assert!(
            parse_mono_frame_side_info(&payload, 0, NeuralNetworkType::Basic, false, None,)
                .is_err()
        );
    }
}
