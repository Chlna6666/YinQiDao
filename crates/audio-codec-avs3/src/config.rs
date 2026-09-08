use yinqidao_codec_core::CodecError;

use crate::bitreader::BitReader;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioCodingMethod {
    Lossless,
    GeneralFullRate,
}

impl AudioCodingMethod {
    pub const fn audio_codec_id(self) -> u8 {
        match self {
            Self::Lossless => 1,
            Self::GeneralFullRate => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NeuralNetworkType {
    Basic,
    LowComplexity,
    Reserved(u8),
}

impl From<u8> for NeuralNetworkType {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Basic,
            1 => Self::LowComplexity,
            value => Self::Reserved(value),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodingProfile {
    Basic,
    ObjectMetadata,
    Hoa,
    Reserved(u8),
}

impl From<u8> for CodingProfile {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Basic,
            1 => Self::ObjectMetadata,
            2 => Self::Hoa,
            value => Self::Reserved(value),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentType {
    Channel,
    Object,
    Mixed,
    Hoa,
}

impl ContentType {
    fn parse(value: u8) -> Result<Self, CodecError> {
        match value {
            0 => Ok(Self::Channel),
            1 => Ok(Self::Object),
            2 => Ok(Self::Mixed),
            3 => Ok(Self::Hoa),
            _ => Err(CodecError::Unsupported(
                "reserved Audio Vivid content_type in dca3",
            )),
        }
    }

    pub const fn coding_profile(self) -> CodingProfile {
        match self {
            Self::Channel => CodingProfile::Basic,
            Self::Object | Self::Mixed => CodingProfile::ObjectMetadata,
            Self::Hoa => CodingProfile::Hoa,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuantizationResolution {
    Pcm16,
    Pcm24,
    Reserved(u8),
}

impl QuantizationResolution {
    pub const fn bits_per_sample(self) -> Option<u8> {
        match self {
            Self::Pcm16 => Some(16),
            Self::Pcm24 => Some(24),
            Self::Reserved(_) => None,
        }
    }
}

impl From<u8> for QuantizationResolution {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::Pcm16,
            2 => Self::Pcm24,
            value => Self::Reserved(value),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelConfiguration {
    Mono,
    Stereo,
    Surround5_1,
    Surround7_1,
    Foa,
    Surround5_1_2,
    Surround5_1_4,
    Surround7_1_2,
    Surround7_1_4,
    Hoa3,
    Hoa2,
    Reserved(u8),
}

impl ChannelConfiguration {
    pub const fn from_index(index: u8) -> Self {
        match index {
            0x0 => Self::Mono,
            0x1 => Self::Stereo,
            0x2 => Self::Surround5_1,
            0x3 => Self::Surround7_1,
            0x6 => Self::Foa,
            0x7 => Self::Surround5_1_2,
            0x8 => Self::Surround5_1_4,
            0x9 => Self::Surround7_1_2,
            0xA => Self::Surround7_1_4,
            0xB => Self::Hoa3,
            0xC => Self::Hoa2,
            value => Self::Reserved(value),
        }
    }

    pub const fn channels(self) -> Option<u16> {
        match self {
            Self::Mono => Some(1),
            Self::Stereo => Some(2),
            Self::Surround5_1 => Some(6),
            Self::Surround7_1 => Some(8),
            Self::Foa => Some(4),
            Self::Surround5_1_2 => Some(8),
            Self::Surround5_1_4 | Self::Surround7_1_2 => Some(10),
            Self::Surround7_1_4 => Some(12),
            Self::Hoa3 => Some(16),
            Self::Hoa2 => Some(9),
            Self::Reserved(_) => None,
        }
    }
}

pub const fn full_rate_sample_rate(index: u8) -> Option<u32> {
    match index {
        0x0 => Some(192_000),
        0x1 => Some(96_000),
        0x2 => Some(48_000),
        0x3 => Some(44_100),
        _ => None,
    }
}

/// Resolve a lossless sampling frequency from the AASF/AATF index and optional extension.
///
/// Indices 0..=3 use the same normative frequency table as general full-rate coding. Lossless
/// alone may use 0xF as an extension marker followed by a 24-bit explicit frequency.
pub(crate) const fn lossless_sample_rate(
    index: u8,
    explicit_sample_rate: Option<u32>,
) -> Option<u32> {
    match index {
        0x0..=0x3 => full_rate_sample_rate(index),
        0xF => explicit_sample_rate,
        _ => None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneralFullRateConfig {
    pub sampling_frequency_index: u8,
    pub sample_rate: Option<u32>,
    pub nn_type: NeuralNetworkType,
    pub content_type: ContentType,
    pub channel_number_index: Option<u8>,
    pub channel_configuration: Option<ChannelConfiguration>,
    /// Semantic number of objects. The `dca3` field already carries the actual count.
    pub number_objects: Option<u8>,
    /// Semantic HOA order. `dca3.hoa_order` equals AATF `order + 1`.
    pub hoa_order: Option<u8>,
    pub total_bitrate_kbps: u16,
    pub resolution: QuantizationResolution,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LosslessConfig {
    pub sampling_frequency_index: u8,
    pub explicit_sample_rate: Option<u32>,
    pub anc_data_index: bool,
    pub coding_profile: CodingProfile,
    pub channel_number: u8,
    pub resolution: QuantizationResolution,
    pub additional_info: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Avs3SpecificConfig {
    GeneralFullRate(GeneralFullRateConfig),
    Lossless(LosslessConfig),
}

impl Avs3SpecificConfig {
    pub const fn coding_method(&self) -> AudioCodingMethod {
        match self {
            Self::GeneralFullRate(_) => AudioCodingMethod::GeneralFullRate,
            Self::Lossless(_) => AudioCodingMethod::Lossless,
        }
    }

    pub fn sample_rate(&self) -> Option<u32> {
        match self {
            Self::GeneralFullRate(config) => config.sample_rate,
            Self::Lossless(config) => lossless_sample_rate(
                config.sampling_frequency_index,
                config.explicit_sample_rate,
            ),
        }
    }

    pub fn bits_per_sample(&self) -> Option<u8> {
        match self {
            Self::GeneralFullRate(config) => config.resolution.bits_per_sample(),
            Self::Lossless(config) => config.resolution.bits_per_sample(),
        }
    }
}

pub fn parse_dca3(payload: &[u8]) -> Result<Avs3SpecificConfig, CodecError> {
    let mut reader = BitReader::new(payload);
    let audio_codec_id = reader.read_bits(4)? as u8;
    match audio_codec_id {
        1 => parse_lossless(&mut reader).map(Avs3SpecificConfig::Lossless),
        2 => parse_general_full_rate(&mut reader).map(Avs3SpecificConfig::GeneralFullRate),
        _ => Err(CodecError::Unsupported(
            "reserved audio_codec_id in Audio Vivid dca3",
        )),
    }
}

fn parse_general_full_rate(reader: &mut BitReader<'_>) -> Result<GeneralFullRateConfig, CodecError> {
    let sampling_frequency_index = reader.read_bits(4)? as u8;
    let nn_type = NeuralNetworkType::from(reader.read_bits(3)? as u8);
    reader.skip_bits(1)?;
    let content_type = ContentType::parse(reader.read_bits(4)? as u8)?;

    let mut channel_number_index = None;
    let mut channel_configuration = None;
    let mut number_objects = None;
    let mut hoa_order = None;

    match content_type {
        ContentType::Channel => {
            let index = reader.read_bits(7)? as u8;
            reader.skip_bits(1)?;
            channel_number_index = Some(index);
            channel_configuration = Some(ChannelConfiguration::from_index(index));
        }
        ContentType::Object => {
            number_objects = Some(reader.read_bits(7)? as u8);
            reader.skip_bits(1)?;
        }
        ContentType::Mixed => {
            let index = reader.read_bits(7)? as u8;
            reader.skip_bits(1)?;
            channel_number_index = Some(index);
            channel_configuration = Some(ChannelConfiguration::from_index(index));
            number_objects = Some(reader.read_bits(7)? as u8);
            reader.skip_bits(1)?;
        }
        ContentType::Hoa => {
            hoa_order = Some((reader.read_bits(4)? as u8).saturating_add(1));
        }
    }

    let total_bitrate_kbps = reader.read_bits(16)? as u16;
    let resolution = QuantizationResolution::from(reader.read_bits(2)? as u8);
    if content_type == ContentType::Hoa {
        reader.skip_bits(2)?;
    } else {
        reader.skip_bits(6)?;
    }

    Ok(GeneralFullRateConfig {
        sampling_frequency_index,
        sample_rate: full_rate_sample_rate(sampling_frequency_index),
        nn_type,
        content_type,
        channel_number_index,
        channel_configuration,
        number_objects,
        hoa_order,
        total_bitrate_kbps,
        resolution,
    })
}

fn parse_lossless(reader: &mut BitReader<'_>) -> Result<LosslessConfig, CodecError> {
    let sampling_frequency_index = reader.read_bits(4)? as u8;
    let explicit_sample_rate = if sampling_frequency_index == 0xF {
        let value = reader.read_bits(24)?;
        if value == 0xFF_FFFF {
            return Err(CodecError::InvalidData(
                "reserved explicit lossless sampling frequency",
            ));
        }
        Some(value)
    } else {
        if full_rate_sample_rate(sampling_frequency_index).is_none() {
            return Err(CodecError::Unsupported(
                "reserved lossless sampling_frequency_index in dca3",
            ));
        }
        None
    };
    let anc_data_index = reader.read_bit()?;
    let coding_profile = CodingProfile::from(reader.read_bits(3)? as u8);
    let channel_number = reader.read_bits(8)? as u8;
    let resolution = QuantizationResolution::from(reader.read_bits(2)? as u8);
    let additional_info_length = reader.read_bits(16)? as usize;
    if reader.bits_remaining() < additional_info_length.saturating_mul(8).saturating_add(2) {
        return Err(CodecError::Truncated);
    }
    let mut additional_info = Vec::with_capacity(additional_info_length);
    for _ in 0..additional_info_length {
        additional_info.push(reader.read_bits(8)? as u8);
    }
    reader.skip_bits(2)?;

    Ok(LosslessConfig {
        sampling_frequency_index,
        explicit_sample_rate,
        anc_data_index,
        coding_profile,
        channel_number,
        resolution,
        additional_info,
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
                    let index = self.bytes.len() - 1;
                    self.bytes[index] |= 1 << (7 - (self.bit_pos & 7));
                }
                self.bit_pos += 1;
            }
        }
    }

    #[test]
    fn parses_full_rate_7_1_4_config() {
        let mut writer = BitWriter::new();
        writer.push(2, 4);
        writer.push(3, 4);
        writer.push(1, 3);
        writer.push(0, 1);
        writer.push(0, 4);
        writer.push(0xA, 7);
        writer.push(0, 1);
        writer.push(832, 16);
        writer.push(2, 2);
        writer.push(0, 6);

        let Avs3SpecificConfig::GeneralFullRate(config) = parse_dca3(&writer.bytes).unwrap() else {
            panic!("full-rate config");
        };
        assert_eq!(config.sample_rate, Some(44_100));
        assert_eq!(
            config.channel_configuration,
            Some(ChannelConfiguration::Surround7_1_4)
        );
        assert_eq!(config.channel_configuration.unwrap().channels(), Some(12));
        assert_eq!(config.total_bitrate_kbps, 832);
        assert_eq!(config.resolution.bits_per_sample(), Some(24));
        assert_eq!(config.content_type.coding_profile(), CodingProfile::Basic);
    }

    #[test]
    fn normalizes_dca3_hoa_order_to_semantic_order() {
        let mut writer = BitWriter::new();
        writer.push(2, 4);
        writer.push(2, 4);
        writer.push(0, 3);
        writer.push(0, 1);
        writer.push(3, 4);
        writer.push(2, 4); // AATF order value 2 means third-order HOA.
        writer.push(768, 16);
        writer.push(2, 2);
        writer.push(0, 2);

        let Avs3SpecificConfig::GeneralFullRate(config) = parse_dca3(&writer.bytes).unwrap() else {
            panic!("full-rate config");
        };
        assert_eq!(config.content_type, ContentType::Hoa);
        assert_eq!(config.hoa_order, Some(3));
    }

    #[test]
    fn parses_lossless_explicit_frequency_and_additional_info() {
        let mut writer = BitWriter::new();
        writer.push(1, 4);
        writer.push(0xF, 4);
        writer.push(88_200, 24);
        writer.push(1, 1);
        writer.push(0, 3);
        writer.push(18, 8);
        writer.push(1, 2);
        writer.push(2, 16);
        writer.push(0xAB, 8);
        writer.push(0xCD, 8);
        writer.push(0, 2);

        let config = parse_dca3(&writer.bytes).unwrap();
        let Avs3SpecificConfig::Lossless(lossless) = &config else {
            panic!("lossless config");
        };
        assert_eq!(lossless.explicit_sample_rate, Some(88_200));
        assert_eq!(config.sample_rate(), Some(88_200));
        assert_eq!(lossless.channel_number, 18);
        assert_eq!(lossless.additional_info, [0xAB, 0xCD]);
    }

    #[test]
    fn resolves_indexed_lossless_frequency() {
        let mut writer = BitWriter::new();
        writer.push(1, 4);
        writer.push(0x2, 4); // 48 kHz
        writer.push(0, 1);
        writer.push(0, 3);
        writer.push(2, 8);
        writer.push(1, 2);
        writer.push(0, 16);
        writer.push(0, 2);

        let config = parse_dca3(&writer.bytes).unwrap();
        let Avs3SpecificConfig::Lossless(lossless) = &config else {
            panic!("lossless config");
        };
        assert_eq!(lossless.explicit_sample_rate, None);
        assert_eq!(config.sample_rate(), Some(48_000));
    }

    #[test]
    fn rejects_reserved_lossless_frequency_index() {
        let mut writer = BitWriter::new();
        writer.push(1, 4);
        writer.push(0x4, 4);

        assert_eq!(
            parse_dca3(&writer.bytes),
            Err(CodecError::Unsupported(
                "reserved lossless sampling_frequency_index in dca3"
            ))
        );
    }
}
