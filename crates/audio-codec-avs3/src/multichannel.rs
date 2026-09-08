use yinqidao_codec_core::CodecError;

use crate::bitreader::BitReader;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MultichannelPairSideInfo {
    /// Raw `channelPairIndex` value. Pair-to-channel semantic mapping is intentionally kept
    /// separate from bitstream parsing because the standard defines that mapping in the MCAC
    /// channel-ordering stage, not in the field width itself.
    pub pair_index: u16,
    pub ild_first: u8,
    pub ild_second: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultichannelSideInfo {
    pub has_silence: bool,
    /// Silence flags for the `coupleChNum` channels (all channels except LFE).
    pub silence_flags: Vec<bool>,
    pub pair_count: u8,
    pub pair_index_bits: u8,
    pub pairs: Vec<MultichannelPairSideInfo>,
    /// Six-bit allocation ratios for non-silent coupled channels. Silent entries remain `None`.
    pub channel_bit_ratios: Vec<Option<u8>>,
    pub next_bit_offset: usize,
}

/// Exact width from table 21 note 1:
/// `floor(log2(coupleChNum * (coupleChNum-1) / 2 - 1)) + 1`, equivalent to ceil(log2(C)).
pub fn channel_pair_index_bits(couple_ch_num: u16) -> Result<u8, CodecError> {
    if couple_ch_num < 2 {
        return Err(CodecError::InvalidData(
            "multichannel pairing requires at least two coupled channels",
        ));
    }
    let combinations = u32::from(couple_ch_num).saturating_mul(u32::from(couple_ch_num - 1)) / 2;
    if combinations <= 1 {
        return Ok(0);
    }
    Ok((u32::BITS - (combinations - 1).leading_zeros()) as u8)
}

/// Parse GY/T 363-2023 table 21 `DecodeMcSideBits()`.
///
/// `couple_ch_num` must exclude LFE. The parser preserves raw `channelPairIndex` values and the
/// two ILD indices; later MCAC upmix code resolves the raw pair index against the normative channel
/// ordering. This keeps bitstream consumption independent from renderer/channel-layout policy.
pub fn parse_multichannel_side_info_at(
    bytes: &[u8],
    bit_offset: usize,
    couple_ch_num: u16,
) -> Result<MultichannelSideInfo, CodecError> {
    if couple_ch_num == 0 {
        return Err(CodecError::InvalidData(
            "multichannel side information has zero coupled channels",
        ));
    }

    let mut reader = BitReader::with_bit_position(bytes, bit_offset)?;
    let has_silence = reader.read_bit()?;
    let mut silence_flags = vec![false; usize::from(couple_ch_num)];
    if has_silence {
        for flag in &mut silence_flags {
            *flag = reader.read_bit()?;
        }
    }

    let pair_count = reader.read_bits(4)? as u8;
    let max_disjoint_pairs = couple_ch_num / 2;
    if u16::from(pair_count) > max_disjoint_pairs {
        return Err(CodecError::InvalidData(
            "multichannel pair count exceeds disjoint channel capacity",
        ));
    }

    let pair_index_bits = if pair_count == 0 {
        0
    } else {
        channel_pair_index_bits(couple_ch_num)?
    };
    let pair_combinations =
        u32::from(couple_ch_num).saturating_mul(u32::from(couple_ch_num.saturating_sub(1))) / 2;

    let mut pairs = Vec::with_capacity(usize::from(pair_count));
    for _ in 0..pair_count {
        let pair_index = if pair_index_bits == 0 {
            0
        } else {
            reader.read_bits(pair_index_bits)? as u16
        };
        if u32::from(pair_index) >= pair_combinations {
            return Err(CodecError::InvalidData(
                "channelPairIndex exceeds available channel-pair combinations",
            ));
        }
        pairs.push(MultichannelPairSideInfo {
            pair_index,
            ild_first: reader.read_bits(5)? as u8,
            ild_second: reader.read_bits(5)? as u8,
        });
    }

    let mut channel_bit_ratios = vec![None; usize::from(couple_ch_num)];
    for (silent, ratio) in silence_flags.iter().zip(&mut channel_bit_ratios) {
        if !*silent {
            *ratio = Some(reader.read_bits(6)? as u8);
        }
    }

    Ok(MultichannelSideInfo {
        has_silence,
        silence_flags,
        pair_count,
        pair_index_bits,
        pairs,
        channel_bit_ratios,
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
    }

    #[test]
    fn pair_index_width_matches_common_layouts() {
        assert_eq!(channel_pair_index_bits(5).unwrap(), 4); // 5.1 excludes LFE: C(5,2)=10
        assert_eq!(channel_pair_index_bits(7).unwrap(), 5); // 7.1 excludes LFE: 21
        assert_eq!(channel_pair_index_bits(11).unwrap(), 6); // 7.1.4 excludes LFE: 55
    }

    #[test]
    fn parses_7_1_4_side_info_without_silence() {
        let couple_ch_num = 11;
        let mut writer = BitWriter::new();
        writer.push(0, 1); // HasSilFlag
        writer.push(2, 4); // pairCnt
        writer.push(3, 6);
        writer.push(5, 5);
        writer.push(7, 5);
        writer.push(42, 6);
        writer.push(8, 5);
        writer.push(9, 5);
        for ratio in 0..couple_ch_num {
            writer.push((ratio * 3 % 64) as u32, 6);
        }
        let expected_end = writer.bit_pos;

        let info = parse_multichannel_side_info_at(&writer.bytes, 0, couple_ch_num).unwrap();
        assert!(!info.has_silence);
        assert_eq!(info.pair_index_bits, 6);
        assert_eq!(info.pair_count, 2);
        assert_eq!(info.pairs[0].pair_index, 3);
        assert_eq!(info.pairs[1].pair_index, 42);
        assert_eq!(info.pairs[0].ild_first, 5);
        assert_eq!(info.pairs[0].ild_second, 7);
        assert!(info.channel_bit_ratios.iter().all(Option::is_some));
        assert_eq!(info.next_bit_offset, expected_end);
    }

    #[test]
    fn silent_channels_omit_bit_ratio_fields() {
        let mut writer = BitWriter::new();
        writer.push(1, 1); // HasSilFlag
        writer.push(0b01010, 5); // five silence flags
        writer.push(0, 4); // no pairs
        // non-silent channels 0,2,4 only
        writer.push(1, 6);
        writer.push(2, 6);
        writer.push(3, 6);

        let info = parse_multichannel_side_info_at(&writer.bytes, 0, 5).unwrap();
        assert_eq!(info.silence_flags, vec![false, true, false, true, false]);
        assert_eq!(
            info.channel_bit_ratios,
            vec![Some(1), None, Some(2), None, Some(3)]
        );
    }

    #[test]
    fn rejects_pair_count_above_disjoint_capacity() {
        let bytes = [0b0_0011_000]; // no silence, pairCnt=3 for 5 channels; max is 2
        assert_eq!(
            parse_multichannel_side_info_at(&bytes, 0, 5),
            Err(CodecError::InvalidData(
                "multichannel pair count exceeds disjoint channel capacity"
            ))
        );
    }
}
