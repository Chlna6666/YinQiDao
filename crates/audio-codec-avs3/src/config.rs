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
    Pcm8,
    Pcm16,
    Pcm24,
    Reserved(u8),
}

impl QuantizationResolution {
    pub const fn bits_per_sample(self) -> Option<u8> {
        match self {
            Self::Pcm8 => Some(8),
            Self::Pcm16 => Some(16),
            Self::Pcm24 => Some(24),
            Self::Reserved(_) => None,
        }
    }
}

impl From<u8> for QuantizationResolution {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Pcm8,
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
    Surround4_0,
    Surround5_1_2,
    Surround5_1_4,
    Surround7_1_2,
    Surround7_1_4,
    Reserved(u8),
}

impl ChannelConfiguration {
    pub const fn from_index(index: u8) -> Self {
        match index {
            0x0 => Self::Mono,
            0x1 => Self::Stereo,
            0x2 => Self::Surround5_1,
            0x3 => Self::Surround7_1,
            // AVS3 GA channel-based coding uses index 6 for a discrete 4.0 bed. FOA/HOA is
            // represented by the dedicated HOA coding profile and must not be inferred here.
            0x6 => Self::Surround4_0,
            0x7 => Self::Surround5_1_2,
            0x8 => Self::Surround5_1_4,
            0x9 => Self::Surround7_1_2,
            0xA => Self::Surround7_1_4,
            value => Self::Reserved(value),
        }
    }

    pub const fn channels(self) -> Option<u16> {
        match self {
            Self::Mono => Some(1),
            Self::Stereo => Some(2),
            Self::Surround5_1 => Some(6),
            Self::Surround7_1 | Self::Surround5_1_2 => Some(8),
            Self::Surround4_0 => Some(4),
            Self::Surround5_1_4 | Self::Surround7_1_2 => Some(10),
            Self::Surround7_1_4 => Some(12),
            Self::Reserved(_) => None,
        }
    }
}

/// General-full-rate AVS3 sampling-frequency table.
///
/// Current Audio Vivid carriage/reference implementations define indices 0..=8. Lossless keeps
/// its stricter 0..=3 + explicit-frequency (0xF) syntax and therefore does not reuse the extended
/// part of this table when validating a Lossless `dca3`.
pub const fn full_rate_sample_rate(index: u8) -> Option<u32> {
    match index {
        0x0 => Some(192_000),
        0x1 => Some(96_000),
        0x2 => Some(48_000),
        0x3 => Some(44_100),
        0x4 => Some(32_000),
        0x5 => Some(24_000),
        0x6 => Some(22_050),
        0x7 => Some(16_000),
        0x8 => Some(8_000),
        _ => None,
    }
}

/// Resolve a lossless sampling frequency from the AASF/AATF index and optional extension.
///
/// Indices 0..=3 use the legacy/full-rate core frequency table. Lossless alone may use 0xF as an
/// extension marker followed by a 24-bit explicit frequency; indices 4..=14 remain reserved here.
pub(crate) const fn lossless_sample_rate(
    index: u8,
    explicit_sample_rate: Option<u32>,
) -> Option<u32> {
    match index {
        0x0 => Some(192_000),
        0x1 => Some(96_000),
        0x2 => Some(48_000),
        0x3 => Some(44_100),
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
    /// Semantic HOA order. `dca3.hoa_order` already equals AATF `order + 1`.
    pub hoa_order: Option<u8>,
    pub total_bitrate_kbps: u16,
    pub resolution: QuantizationResolution,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LosslessConfig {
    pub sampling_frequency_index: u8,
    pub explicit_sample_rate: Option<u32>,
    /// GY/T 363-2023 defines this one-bit value as the presence flag for ancillary data.
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
            Self::Lossless(config) => {
                lossless_sample_rate(config.sampling_frequency_index, config.explicit_sample_rate)
            }
        }
    }

    pub fn bits_per_sample(&self) -> Option<u8> {
        match self {
            Self::GeneralFullRate(config) => config.resolution.bits_per_sample(),
            Self::Lossless(config) => config.resolution.bits_per_sample(),
        }
    }

    /// Resolve the output-signal count exclusively from `dca3` semantics. AV3A decoders must not
    /// rely on the legacy AudioSampleEntry ChannelCount field because CA3SpecificBox supersedes it.
    pub fn channels(&self) -> Option<u16> {
        match self {
            Self::GeneralFullRate(config) => match config.content_type {
                ContentType::Channel => config.channel_configuration?.channels(),
                ContentType::Object => config.number_objects.map(u16::from),
                ContentType::Mixed => config
                    .channel_configuration?
                    .channels()
                    .zip(config.number_objects.map(u16::from))
                    .map(|(bed, objects)| bed.saturating_add(objects)),
                ContentType::Hoa => config.hoa_order.map(|order| {
                    let side = u16::from(order).saturating_add(1);
                    side.saturating_mul(side)
                }),
            },
            Self::Lossless(config) => Some(u16::from(config.channel_number)),
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

fn parse_general_full_rate(
    reader: &mut BitReader<'_>,
) -> Result<GeneralFullRateConfig, CodecError> {
    let sampling_frequency_index = reader.read_bits(4)? as u8;
    let sample_rate = full_rate_sample_rate(sampling_frequency_index).ok_or(
        CodecError::Unsupported("reserved general-full-rate sampling_frequency_index in dca3"),
    )?;
    let nn_type = NeuralNetworkType::from(reader.read_bits(3)? as u8);
    if matches!(nn_type, NeuralNetworkType::Reserved(_)) {
        return Err(CodecError::Unsupported(
            "reserved general-full-rate nn_type in dca3",
        ));
    }
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
            let configuration = ChannelConfiguration::from_index(index);
            if matches!(configuration, ChannelConfiguration::Reserved(_)) {
                return Err(CodecError::Unsupported(
                    "reserved channel_number_index in Audio Vivid dca3",
                ));
            }
            channel_number_index = Some(index);
            channel_configuration = Some(configuration);
        }
        ContentType::Object => {
            let objects = reader.read_bits(7)? as u8;
            reader.skip_bits(1)?;
            if objects == 0 {
                return Err(CodecError::InvalidData(
                    "object Audio Vivid dca3 declares zero objects",
                ));
            }
            number_objects = Some(objects);
        }
        ContentType::Mixed => {
            let index = reader.read_bits(7)? as u8;
            reader.skip_bits(1)?;
            let configuration = ChannelConfiguration::from_index(index);
            if matches!(configuration, ChannelConfiguration::Reserved(_))
                || configuration == ChannelConfiguration::Mono
            {
                return Err(CodecError::Unsupported(
                    "reserved sound-bed channel_number_index in Audio Vivid dca3",
                ));
            }
            channel_number_index = Some(index);
            channel_configuration = Some(configuration);
            let objects = reader.read_bits(7)? as u8;
            reader.skip_bits(1)?;
            if objects == 0 {
                return Err(CodecError::InvalidData(
                    "mixed Audio Vivid dca3 declares zero objects",
                ));
            }
            number_objects = Some(objects);
        }
        ContentType::Hoa => {
            let order = reader.read_bits(4)? as u8;
            if !(1..=3).contains(&order) {
                return Err(CodecError::Unsupported(
                    "reserved HOA order in Audio Vivid dca3",
                ));
            }
            // Unlike AATF, `dca3.hoa_order` already carries the semantic order (`order + 1`).
            hoa_order = Some(order);
        }
    }

    let total_bitrate_kbps = reader.read_bits(16)? as u16;
    if total_bitrate_kbps == 0 {
        return Err(CodecError::InvalidData(
            "Audio Vivid dca3 total_bitrate must be non-zero",
        ));
    }
    let resolution = QuantizationResolution::from(reader.read_bits(2)? as u8);
    if matches!(resolution, QuantizationResolution::Reserved(_)) {
        return Err(CodecError::Unsupported(
            "reserved general-full-rate resolution in dca3",
        ));
    }
    if content_type == ContentType::Hoa {
        reader.skip_bits(2)?;
    } else {
        reader.skip_bits(6)?;
    }

    Ok(GeneralFullRateConfig {
        sampling_frequency_index,
        sample_rate: Some(sample_rate),
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
        if lossless_sample_rate(sampling_frequency_index, None).is_none() {
            return Err(CodecError::Unsupported(
                "reserved lossless sampling_frequency_index in dca3",
            ));
        }
        None
    };
    let anc_data_index = reader.read_bit()?;
    let coding_profile = CodingProfile::from(reader.read_bits(3)? as u8);
    if matches!(coding_profile, CodingProfile::Reserved(_)) {
        return Err(CodecError::Unsupported(
            "reserved lossless coding_profile in dca3",
        ));
    }
    let channel_number = reader.read_bits(8)? as u8;
    let resolution = QuantizationResolution::from(reader.read_bits(2)? as u8);
    if !matches!(
        resolution,
        QuantizationResolution::Pcm16 | QuantizationResolution::Pcm24
    ) {
        return Err(CodecError::Unsupported(
            "reserved lossless resolution in dca3",
        ));
    }
    let additional_info_length = reader.read_bits(16)? as usize;
    if reader.bits_remaining() < additional_info_length.saturating_mul(8).saturating_add(2) {
        return Err(CodecError::Truncated);
    }
    let mut additional_info = Vec::with_capacity(additional_info_length);
    for _ in 0..additional_info_length {
        additional_info.push(reader.read_bits(8)? as u8);
    }
    // GY/T 420-2025 requires reserved bits to be set to one, but decoders shall ignore them.
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
    fn accepts_extended_full_rate_sampling_frequencies() {
        for (index, sample_rate) in [
            (0, 192_000),
            (1, 96_000),
            (2, 48_000),
            (3, 44_100),
            (4, 32_000),
            (5, 24_000),
            (6, 22_050),
            (7, 16_000),
            (8, 8_000),
        ] {
            assert_eq!(full_rate_sample_rate(index), Some(sample_rate));
        }
        assert_eq!(full_rate_sample_rate(9), None);
    }

    #[test]
    fn channel_index_six_is_discrete_4_0_not_foa() {
        assert_eq!(
            ChannelConfiguration::from_index(6),
            ChannelConfiguration::Surround4_0
        );
        assert_eq!(ChannelConfiguration::from_index(6).channels(), Some(4));
        assert!(matches!(
            ChannelConfiguration::from_index(0xB),
            ChannelConfiguration::Reserved(0xB)
        ));
    }

    #[test]
    fn dca3_hoa_order_is_already_semantic() {
        let mut writer = BitWriter::new();
        writer.push(2, 4);
        writer.push(2, 4);
        writer.push(0, 3);
        writer.push(0, 1);
        writer.push(3, 4);
        writer.push(3, 4); // dca3 carries semantic third-order HOA.
        writer.push(768, 16);
        writer.push(2, 2);
        writer.push(0, 2);

        let config = parse_dca3(&writer.bytes).unwrap();
        let Avs3SpecificConfig::GeneralFullRate(ga) = &config else {
            panic!("full-rate config");
        };
        assert_eq!(ga.content_type, ContentType::Hoa);
        assert_eq!(ga.hoa_order, Some(3));
        assert_eq!(config.channels(), Some(16));
    }

    #[test]
    fn resolves_dca3_channel_object_and_hoa_counts() {
        let mut channel = BitWriter::new();
        channel.push(2, 4);
        channel.push(2, 4);
        channel.push(0, 3);
        channel.push(0, 1);
        channel.push(0, 4);
        channel.push(8, 7);
        channel.push(0, 1);
        channel.push(704, 16);
        channel.push(2, 2);
        channel.push(0, 6);
        assert_eq!(parse_dca3(&channel.bytes).unwrap().channels(), Some(10));

        let mut objects = BitWriter::new();
        objects.push(2, 4);
        objects.push(2, 4);
        objects.push(0, 3);
        objects.push(0, 1);
        objects.push(1, 4);
        objects.push(6, 7);
        objects.push(0, 1);
        objects.push(384, 16);
        objects.push(2, 2);
        objects.push(0, 6);
        assert_eq!(parse_dca3(&objects.bytes).unwrap().channels(), Some(6));

        let mut mixed = BitWriter::new();
        mixed.push(2, 4);
        mixed.push(2, 4);
        mixed.push(0, 3);
        mixed.push(0, 1);
        mixed.push(2, 4);
        mixed.push(2, 7);
        mixed.push(0, 1);
        mixed.push(2, 7);
        mixed.push(0, 1);
        mixed.push(576, 16);
        mixed.push(2, 2);
        mixed.push(0, 6);
        assert_eq!(parse_dca3(&mixed.bytes).unwrap().channels(), Some(8));
    }

    #[test]
    fn parses_lossless_explicit_frequency_and_additional_info() {
        let mut writer = BitWriter::new();
        writer.push(1, 4);
        writer.push(0xF, 4);
        writer.push(88_200, 24);
        writer.push(0, 1);
        writer.push(0, 3);
        writer.push(18, 8);
        writer.push(1, 2);
        writer.push(2, 16);
        writer.push(0xAB, 8);
        writer.push(0xCD, 8);
        writer.push(3, 2);

        let config = parse_dca3(&writer.bytes).unwrap();
        let Avs3SpecificConfig::Lossless(lossless) = &config else {
            panic!("lossless config");
        };
        assert_eq!(lossless.explicit_sample_rate, Some(88_200));
        assert_eq!(config.sample_rate(), Some(88_200));
        assert!(!lossless.anc_data_index);
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
        writer.push(3, 2);

        let config = parse_dca3(&writer.bytes).unwrap();
        let Avs3SpecificConfig::Lossless(lossless) = &config else {
            panic!("lossless config");
        };
        assert_eq!(lossless.explicit_sample_rate, None);
        assert_eq!(config.sample_rate(), Some(48_000));
    }

    #[test]
    fn accepts_lossless_dca3_ancillary_flag() {
        let mut writer = BitWriter::new();
        writer.push(1, 4);
        writer.push(0x2, 4);
        writer.push(1, 1);
        writer.push(0, 3);
        writer.push(2, 8);
        writer.push(1, 2);
        writer.push(0, 16);
        writer.push(3, 2);

        let Avs3SpecificConfig::Lossless(config) = parse_dca3(&writer.bytes).unwrap() else {
            panic!("lossless config");
        };
        assert!(config.anc_data_index);
    }

    #[test]
    fn rejects_reserved_lossless_coding_profile() {
        let mut writer = BitWriter::new();
        writer.push(1, 4);
        writer.push(0x2, 4);
        writer.push(0, 1);
        writer.push(3, 3);

        assert_eq!(
            parse_dca3(&writer.bytes),
            Err(CodecError::Unsupported(
                "reserved lossless coding_profile in dca3"
            ))
        );
    }

    #[test]
    fn rejects_reserved_lossless_resolution() {
        let mut writer = BitWriter::new();
        writer.push(1, 4);
        writer.push(0x2, 4);
        writer.push(0, 1);
        writer.push(0, 3);
        writer.push(2, 8);
        writer.push(0, 2);

        assert_eq!(
            parse_dca3(&writer.bytes),
            Err(CodecError::Unsupported(
                "reserved lossless resolution in dca3"
            ))
        );
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
