use yinqidao_codec_core::CodecError;

use crate::{
    bitreader::BitReader,
    config::{
        AudioCodingMethod, ChannelConfiguration, CodingProfile, NeuralNetworkType,
        QuantizationResolution, full_rate_sample_rate, lossless_sample_rate,
    },
};

pub const AATF_SYNCWORD: u16 = 0x0FFF;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SoundBedType {
    ObjectsOnly,
    ChannelBedAndObjects,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AatfFrameHeader {
    pub coding_method: AudioCodingMethod,
    pub anc_data_index: bool,
    pub nn_type: Option<NeuralNetworkType>,
    pub coding_profile: CodingProfile,
    pub sampling_frequency_index: u8,
    pub sample_rate: Option<u32>,
    pub raw_frame_length: Option<u16>,
    /// Raw 8-bit `aatf_error_check()` value. CRC verification is a separate decoder milestone.
    pub header_crc: u8,
    pub channel_number: Option<u16>,
    pub channel_number_index: Option<u8>,
    pub channel_configuration: Option<ChannelConfiguration>,
    pub sound_bed_type: Option<SoundBedType>,
    /// Raw 7-bit field; the semantic object-channel count is this value + 1.
    pub object_channel_number: Option<u8>,
    pub bitrate_index_per_channel: Option<u8>,
    /// Semantic HOA/FOA order (`order + 1` from the AATF field).
    pub hoa_order: Option<u8>,
    pub resolution: QuantizationResolution,
    pub bitrate_index: Option<u8>,
    /// General full-rate AATF carries `frame_error_check()` immediately after the decoded header.
    /// Lossless carries its frame check after the raw data block, so it is not consumed here.
    pub frame_crc: Option<u8>,
    /// Byte offset of the raw coded block after the AATF header/check and byte alignment.
    pub payload_offset_bytes: usize,
}

impl AatfFrameHeader {
    pub fn object_channels(&self) -> Option<u16> {
        self.object_channel_number
            .map(|value| u16::from(value).saturating_add(1))
    }

    pub fn resolved_channels(&self) -> Option<u16> {
        match self.coding_method {
            AudioCodingMethod::Lossless => self.channel_number,
            AudioCodingMethod::GeneralFullRate => match self.coding_profile {
                CodingProfile::Basic => self
                    .channel_configuration
                    .and_then(ChannelConfiguration::channels),
                CodingProfile::ObjectMetadata => match self.sound_bed_type {
                    Some(SoundBedType::ObjectsOnly) => self.object_channels(),
                    Some(SoundBedType::ChannelBedAndObjects) => self
                        .channel_configuration
                        .and_then(ChannelConfiguration::channels)
                        .zip(self.object_channels())
                        .map(|(bed, objects)| bed.saturating_add(objects)),
                    None => None,
                },
                CodingProfile::Hoa => self.hoa_order.map(|order| {
                    let side = u16::from(order).saturating_add(1);
                    side.saturating_mul(side)
                }),
                CodingProfile::Reserved(_) => None,
            },
        }
    }
}

/// Parse the synchronization word and normative AATF decoded-header fields from one AV3A sample.
///
/// ISO-BMFF AVS3-P3 carriage places one AATF frame in each sample. This function stops exactly at
/// the raw coded block and deliberately does not interpret `ga_co_raw_data_block()` yet.
pub fn parse_aatf_frame_header(packet: &[u8]) -> Result<AatfFrameHeader, CodecError> {
    let mut reader = BitReader::new(packet);
    let syncword = reader.read_bits(12)? as u16;
    if syncword != AATF_SYNCWORD {
        return Err(CodecError::InvalidData("invalid AATF syncword"));
    }

    let audio_codec_id = reader.read_bits(4)? as u8;
    let coding_method = match audio_codec_id {
        1 => AudioCodingMethod::Lossless,
        2 => AudioCodingMethod::GeneralFullRate,
        _ => return Err(CodecError::Unsupported("reserved AATF audio_codec_id")),
    };
    let anc_data_index = reader.read_bit()?;

    let nn_type = if coding_method == AudioCodingMethod::GeneralFullRate {
        Some(NeuralNetworkType::from(reader.read_bits(3)? as u8))
    } else {
        None
    };
    let coding_profile = CodingProfile::from(reader.read_bits(3)? as u8);
    if matches!(coding_profile, CodingProfile::Reserved(_)) {
        return Err(CodecError::Unsupported("reserved AATF coding_profile"));
    }

    let sampling_frequency_index = reader.read_bits(4)? as u8;
    let explicit_sample_rate =
        if coding_method == AudioCodingMethod::Lossless && sampling_frequency_index == 0xF {
            let sample_rate = reader.read_bits(24)?;
            if sample_rate == 0xFF_FFFF {
                return Err(CodecError::InvalidData(
                    "reserved explicit AATF sampling frequency",
                ));
            }
            Some(sample_rate)
        } else {
            None
        };
    let sample_rate = match coding_method {
        AudioCodingMethod::GeneralFullRate => full_rate_sample_rate(sampling_frequency_index),
        AudioCodingMethod::Lossless => {
            let sample_rate = lossless_sample_rate(sampling_frequency_index, explicit_sample_rate);
            if sample_rate.is_none() {
                return Err(CodecError::Unsupported(
                    "reserved lossless AATF sampling_frequency_index",
                ));
            }
            sample_rate
        }
    };

    let raw_frame_length = if coding_method != AudioCodingMethod::GeneralFullRate {
        Some(reader.read_bits(16)? as u16)
    } else {
        None
    };

    let header_crc = reader.read_bits(8)? as u8;

    let mut channel_number = None;
    let mut channel_number_index = None;
    let mut channel_configuration = None;
    let mut sound_bed_type = None;
    let mut object_channel_number = None;
    let mut bitrate_index_per_channel = None;
    let mut hoa_order = None;
    let mut bitrate_index = None;

    match coding_method {
        AudioCodingMethod::Lossless => {
            // Annex A encodes values 0..14 directly in four bits; 15 is an extension marker and
            // the actual channel count follows in eight bits.
            let compact = reader.read_bits(4)? as u8;
            channel_number = Some(if compact == 0xF {
                reader.read_bits(8)? as u16
            } else {
                u16::from(compact)
            });
        }
        AudioCodingMethod::GeneralFullRate => match coding_profile {
            CodingProfile::Basic => {
                let index = reader.read_bits(7)? as u8;
                channel_number_index = Some(index);
                channel_configuration = Some(ChannelConfiguration::from_index(index));
            }
            CodingProfile::ObjectMetadata => {
                let raw_sound_bed_type = reader.read_bits(2)? as u8;
                match raw_sound_bed_type {
                    0 => {
                        sound_bed_type = Some(SoundBedType::ObjectsOnly);
                        object_channel_number = Some(reader.read_bits(7)? as u8);
                        bitrate_index_per_channel = Some(reader.read_bits(4)? as u8);
                    }
                    1 => {
                        sound_bed_type = Some(SoundBedType::ChannelBedAndObjects);
                        let index = reader.read_bits(7)? as u8;
                        channel_number_index = Some(index);
                        channel_configuration = Some(ChannelConfiguration::from_index(index));
                        bitrate_index = Some(reader.read_bits(4)? as u8);
                        object_channel_number = Some(reader.read_bits(7)? as u8);
                        bitrate_index_per_channel = Some(reader.read_bits(4)? as u8);
                    }
                    _ => return Err(CodecError::Unsupported("reserved AATF soundBedType")),
                }
            }
            CodingProfile::Hoa => {
                hoa_order = Some((reader.read_bits(4)? as u8).saturating_add(1));
            }
            CodingProfile::Reserved(_) => unreachable!("reserved profile rejected above"),
        },
    }

    let resolution = QuantizationResolution::from(reader.read_bits(2)? as u8);
    if coding_method == AudioCodingMethod::GeneralFullRate
        && coding_profile != CodingProfile::ObjectMetadata
    {
        bitrate_index = Some(reader.read_bits(4)? as u8);
    }

    let frame_crc = if coding_method == AudioCodingMethod::GeneralFullRate {
        Some(reader.read_bits(8)? as u8)
    } else {
        None
    };
    reader.align_byte();

    Ok(AatfFrameHeader {
        coding_method,
        anc_data_index,
        nn_type,
        coding_profile,
        sampling_frequency_index,
        sample_rate,
        raw_frame_length,
        header_crc,
        channel_number,
        channel_number_index,
        channel_configuration,
        sound_bed_type,
        object_channel_number,
        bitrate_index_per_channel,
        hoa_order,
        resolution,
        bitrate_index,
        frame_crc,
        payload_offset_bytes: reader.position_bits() / 8,
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
                    let index = self.bytes.len() - 1;
                    self.bytes[index] |= 1 << (7 - (self.bit_pos & 7));
                }
                self.bit_pos += 1;
            }
        }
    }

    #[test]
    fn parses_44k1_7_1_4_full_rate_header() {
        let mut writer = BitWriter::new();
        writer.push(AATF_SYNCWORD.into(), 12);
        writer.push(2, 4);
        writer.push(0, 1);
        writer.push(1, 3); // low-complexity NN
        writer.push(0, 3); // basic profile
        writer.push(3, 4); // 44.1 kHz
        writer.push(0x5A, 8); // aatf_error_check
        writer.push(0xA, 7); // 7.1.4
        writer.push(2, 2); // 24-bit
        writer.push(0xE, 4);
        writer.push(0xA5, 8); // frame_error_check

        let header = parse_aatf_frame_header(&writer.bytes).unwrap();
        assert_eq!(header.coding_method, AudioCodingMethod::GeneralFullRate);
        assert_eq!(header.sample_rate, Some(44_100));
        assert_eq!(
            header.channel_configuration,
            Some(ChannelConfiguration::Surround7_1_4)
        );
        assert_eq!(header.resolved_channels(), Some(12));
        assert_eq!(header.resolution.bits_per_sample(), Some(24));
        assert_eq!(header.frame_crc, Some(0xA5));
        assert_eq!(header.payload_offset_bytes, 7);
    }

    #[test]
    fn parses_object_only_channel_count() {
        let mut writer = BitWriter::new();
        writer.push(AATF_SYNCWORD.into(), 12);
        writer.push(2, 4);
        writer.push(0, 1);
        writer.push(0, 3);
        writer.push(1, 3); // object metadata profile
        writer.push(2, 4); // 48 kHz
        writer.push(0, 8);
        writer.push(0, 2); // no sound bed
        writer.push(7, 7); // 8 object channels
        writer.push(3, 4);
        writer.push(1, 2); // 16-bit
        writer.push(0, 8);

        let header = parse_aatf_frame_header(&writer.bytes).unwrap();
        assert_eq!(header.sound_bed_type, Some(SoundBedType::ObjectsOnly));
        assert_eq!(header.object_channels(), Some(8));
        assert_eq!(header.resolved_channels(), Some(8));
        assert_eq!(header.sample_rate, Some(48_000));
    }

    #[test]
    fn resolves_mixed_bed_plus_object_channels() {
        let mut writer = BitWriter::new();
        writer.push(AATF_SYNCWORD.into(), 12);
        writer.push(2, 4);
        writer.push(0, 1);
        writer.push(0, 3);
        writer.push(1, 3);
        writer.push(2, 4);
        writer.push(0, 8);
        writer.push(1, 2); // channel bed + objects
        writer.push(2, 7); // 5.1 = 6 channels
        writer.push(4, 4); // bed bitrate index
        writer.push(1, 7); // 2 object channels
        writer.push(2, 4);
        writer.push(1, 2);
        writer.push(0, 8);

        let header = parse_aatf_frame_header(&writer.bytes).unwrap();
        assert_eq!(header.resolved_channels(), Some(8));
        assert_eq!(header.bitrate_index, Some(4));
    }

    #[test]
    fn parses_indexed_lossless_sample_rate() {
        let mut writer = BitWriter::new();
        writer.push(AATF_SYNCWORD.into(), 12);
        writer.push(1, 4); // lossless
        writer.push(0, 1); // no ancillary data
        writer.push(0, 3); // basic profile
        writer.push(2, 4); // 48 kHz
        writer.push(2048, 16); // raw frame length in the coded bitstream
        writer.push(0x5A, 8); // aatf_error_check
        writer.push(2, 4); // two channels
        writer.push(1, 2); // 16-bit

        let header = parse_aatf_frame_header(&writer.bytes).unwrap();
        assert_eq!(header.coding_method, AudioCodingMethod::Lossless);
        assert_eq!(header.sample_rate, Some(48_000));
        assert_eq!(header.raw_frame_length, Some(2048));
        assert_eq!(header.resolved_channels(), Some(2));
        assert_eq!(header.frame_crc, None);
    }

    #[test]
    fn rejects_reserved_lossless_sample_rate_index() {
        let mut writer = BitWriter::new();
        writer.push(AATF_SYNCWORD.into(), 12);
        writer.push(1, 4);
        writer.push(0, 1);
        writer.push(0, 3);
        writer.push(4, 4);

        assert_eq!(
            parse_aatf_frame_header(&writer.bytes),
            Err(CodecError::Unsupported(
                "reserved lossless AATF sampling_frequency_index"
            ))
        );
    }

    #[test]
    fn rejects_wrong_syncword_before_touching_codec_payload() {
        let packet = [0_u8; 16];
        assert_eq!(
            parse_aatf_frame_header(&packet),
            Err(CodecError::InvalidData("invalid AATF syncword"))
        );
    }
}
