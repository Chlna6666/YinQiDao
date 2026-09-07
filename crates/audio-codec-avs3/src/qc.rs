use yinqidao_codec_core::CodecError;

use crate::{NeuralNetworkType, bitreader::BitReader};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BitRange {
    pub bit_offset: usize,
    pub bit_len: usize,
}

impl BitRange {
    pub const fn end_bit_offset(self) -> usize {
        self.bit_offset.saturating_add(self.bit_len)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QcSideInfo {
    pub is_feat_amplified: Option<bool>,
    pub scale_q_idx: Option<u8>,
    pub scale_q_idx_lc: Option<u8>,
    pub nf_param_q_idx: [Option<u8>; 2],
    pub context_num_bytes: u8,
    pub context_bitstream: BitRange,
    pub base_bitstream: BitRange,
    pub channel_bytes: usize,
    pub next_bit_offset: usize,
}

/// Fixed table-14 side-information bits before the two byte-counted range-coded bitstreams.
pub fn qc_fixed_header_bits(
    nn_type: NeuralNetworkType,
    num_groups: u8,
) -> Result<usize, CodecError> {
    if !(1..=2).contains(&num_groups) {
        return Err(CodecError::InvalidData(
            "DecodeQcBits numGroups must be one or two",
        ));
    }
    match nn_type {
        NeuralNetworkType::Basic | NeuralNetworkType::LowComplexity => {
            Ok(16 + usize::from(num_groups) * 3)
        }
        NeuralNetworkType::Reserved(_) => Err(CodecError::Unsupported(
            "reserved AVS3 neural-network type in DecodeQcBits",
        )),
    }
}

/// Parse one complete table-14 `DecodeQcBits()` block without copying range-coded payload bytes.
pub fn parse_qc_side_info_at(
    bytes: &[u8],
    bit_offset: usize,
    nn_type: NeuralNetworkType,
    num_groups: u8,
    channel_bytes: usize,
) -> Result<QcSideInfo, CodecError> {
    qc_fixed_header_bits(nn_type, num_groups)?;
    let mut reader = BitReader::with_bit_position(bytes, bit_offset)?;

    let (is_feat_amplified, scale_q_idx, scale_q_idx_lc) = match nn_type {
        NeuralNetworkType::Basic => (
            Some(reader.read_bit()?),
            Some(reader.read_bits(7)? as u8),
            None,
        ),
        NeuralNetworkType::LowComplexity => {
            (None, None, Some(reader.read_bits(8)? as u8))
        }
        NeuralNetworkType::Reserved(_) => {
            return Err(CodecError::Unsupported(
                "reserved AVS3 neural-network type in DecodeQcBits",
            ));
        }
    };

    let mut nf_param_q_idx = [None; 2];
    for slot in nf_param_q_idx.iter_mut().take(usize::from(num_groups)) {
        *slot = Some(reader.read_bits(3)? as u8);
    }

    let context_num_bytes = reader.read_bits(8)? as u8;
    let context_bytes = usize::from(context_num_bytes);
    if context_bytes > channel_bytes {
        return Err(CodecError::InvalidData(
            "DecodeQcBits contextNumBytes exceeds allocated channelBytes",
        ));
    }

    let context_bitstream = BitRange {
        bit_offset: reader.position_bits(),
        bit_len: context_bytes.saturating_mul(8),
    };
    reader.skip_bits(context_bitstream.bit_len)?;

    let base_bitstream = BitRange {
        bit_offset: reader.position_bits(),
        bit_len: channel_bytes
            .saturating_sub(context_bytes)
            .saturating_mul(8),
    };
    reader.skip_bits(base_bitstream.bit_len)?;

    Ok(QcSideInfo {
        is_feat_amplified,
        scale_q_idx,
        scale_q_idx_lc,
        nf_param_q_idx,
        context_num_bytes,
        context_bitstream,
        base_bitstream,
        channel_bytes,
        next_bit_offset: reader.position_bits(),
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
    }

    #[test]
    fn fixed_header_size_tracks_group_count() {
        assert_eq!(qc_fixed_header_bits(NeuralNetworkType::Basic, 1).unwrap(), 19);
        assert_eq!(qc_fixed_header_bits(NeuralNetworkType::Basic, 2).unwrap(), 22);
        assert_eq!(
            qc_fixed_header_bits(NeuralNetworkType::LowComplexity, 2).unwrap(),
            22
        );
    }

    #[test]
    fn parses_basic_profile_without_byte_alignment_assumption() {
        let mut writer = BitWriter::new();
        writer.push(0b101, 3);
        let start = writer.bit_pos;
        writer.push(1, 1);
        writer.push(77, 7);
        writer.push(3, 3);
        writer.push(5, 3);
        writer.push(2, 8);
        writer.push(0xABCD, 16);
        writer.push(0x123456, 24);
        let expected_end = writer.bit_pos;

        let info = parse_qc_side_info_at(
            &writer.bytes,
            start,
            NeuralNetworkType::Basic,
            2,
            5,
        )
        .unwrap();
        assert_eq!(info.is_feat_amplified, Some(true));
        assert_eq!(info.scale_q_idx, Some(77));
        assert_eq!(info.nf_param_q_idx, [Some(3), Some(5)]);
        assert_eq!(info.context_num_bytes, 2);
        assert_eq!(info.context_bitstream.bit_len, 16);
        assert_eq!(info.base_bitstream.bit_len, 24);
        assert_eq!(info.next_bit_offset, expected_end);
    }

    #[test]
    fn rejects_context_larger_than_channel_allocation() {
        let mut writer = BitWriter::new();
        writer.push(0, 1);
        writer.push(0, 7);
        writer.push(0, 3);
        writer.push(1, 8); // contextNumBytes=1, but channelBytes=0
        assert_eq!(
            parse_qc_side_info_at(
                &writer.bytes,
                0,
                NeuralNetworkType::Basic,
                1,
                0,
            ),
            Err(CodecError::InvalidData(
                "DecodeQcBits contextNumBytes exceeds allocated channelBytes"
            ))
        );
    }
}
