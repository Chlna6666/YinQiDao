use yinqidao_codec_core::CodecError;

use crate::{AatfFrameHeader, AudioCodingMethod, CodingProfile, SoundBedType};

/// General full-rate decoder branch selected from the already decoded AATF header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GaCodecFormat {
    Mono,
    Stereo,
    Multichannel,
    Hoa,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GaDecodePlan {
    pub format: GaCodecFormat,
    /// Number of channel/object/HOA output signals represented by the current frame header.
    ///
    /// This is not the number of internal HOA virtual-speaker/residual channels, which is carried
    /// in HOA side information and is intentionally left for the HOA decoder milestone.
    pub output_channels: Option<u16>,
}

impl GaDecodePlan {
    /// Derive the `codecFormat` switch used by `ga_co_raw_data_block()` from normative AATF fields.
    pub fn from_header(header: &AatfFrameHeader) -> Result<Self, CodecError> {
        if header.coding_method != AudioCodingMethod::GeneralFullRate {
            return Err(CodecError::InvalidData(
                "general full-rate decode plan requested for a non-GA AATF frame",
            ));
        }

        let format = match header.coding_profile {
            CodingProfile::Basic => match header.channel_number_index.ok_or(
                CodecError::InvalidData("basic AVS3 profile is missing channel_number_index"),
            )? {
                0 => GaCodecFormat::Mono,
                1 => GaCodecFormat::Stereo,
                _ => GaCodecFormat::Multichannel,
            },
            CodingProfile::ObjectMetadata => match header.sound_bed_type.ok_or(
                CodecError::InvalidData("object AVS3 profile is missing soundBedType"),
            )? {
                SoundBedType::ObjectsOnly => {
                    match header.object_channel_number.ok_or(CodecError::InvalidData(
                        "object-only AVS3 frame is missing object_channel_number",
                    ))? {
                        0 => GaCodecFormat::Mono,
                        1 => GaCodecFormat::Stereo,
                        _ => GaCodecFormat::Multichannel,
                    }
                }
                // The 2023 profile requires at least three aggregate signals for mixed content and
                // explicitly reuses the multichannel decoder path.
                SoundBedType::ChannelBedAndObjects => GaCodecFormat::Multichannel,
            },
            CodingProfile::Hoa => GaCodecFormat::Hoa,
            CodingProfile::Reserved(_) => {
                return Err(CodecError::Unsupported("reserved AVS3 coding profile"));
            }
        };

        Ok(Self {
            format,
            output_channels: header.resolved_channels(),
        })
    }
}

/// Return the general/lossless coded block following the already parsed AATF header.
///
/// The frame parser computes this offset after the general full-rate frame CRC and byte alignment.
/// Metadata remains at the beginning of this returned slice, exactly as specified by
/// `ga_co_raw_data_block()`, so callers cannot accidentally bypass metadata parsing.
pub fn coded_payload<'a>(
    packet: &'a [u8],
    header: &AatfFrameHeader,
) -> Result<&'a [u8], CodecError> {
    packet
        .get(header.payload_offset_bytes..)
        .ok_or(CodecError::Truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChannelConfiguration, NeuralNetworkType, QuantizationResolution};

    fn header(profile: CodingProfile) -> AatfFrameHeader {
        AatfFrameHeader {
            coding_method: AudioCodingMethod::GeneralFullRate,
            anc_data_index: false,
            nn_type: Some(NeuralNetworkType::Basic),
            coding_profile: profile,
            sampling_frequency_index: 2,
            sample_rate: Some(48_000),
            raw_frame_length: None,
            header_crc: 0,
            channel_number: None,
            channel_number_index: None,
            channel_configuration: None,
            sound_bed_type: None,
            object_channel_number: None,
            bitrate_index_per_channel: None,
            hoa_order: None,
            resolution: QuantizationResolution::Pcm24,
            bitrate_index: Some(0),
            frame_crc: Some(0),
            payload_offset_bytes: 7,
        }
    }

    #[test]
    fn routes_basic_profiles_by_channel_index() {
        let mut mono = header(CodingProfile::Basic);
        mono.channel_number_index = Some(0);
        mono.channel_configuration = Some(ChannelConfiguration::Mono);
        assert_eq!(
            GaDecodePlan::from_header(&mono).unwrap().format,
            GaCodecFormat::Mono
        );

        let mut stereo = header(CodingProfile::Basic);
        stereo.channel_number_index = Some(1);
        stereo.channel_configuration = Some(ChannelConfiguration::Stereo);
        assert_eq!(
            GaDecodePlan::from_header(&stereo).unwrap().format,
            GaCodecFormat::Stereo
        );

        let mut surround = header(CodingProfile::Basic);
        surround.channel_number_index = Some(0xA);
        surround.channel_configuration = Some(ChannelConfiguration::Surround7_1_4);
        let plan = GaDecodePlan::from_header(&surround).unwrap();
        assert_eq!(plan.format, GaCodecFormat::Multichannel);
        assert_eq!(plan.output_channels, Some(12));
    }

    #[test]
    fn routes_object_and_mixed_profiles_without_guessing_payload_bits() {
        let mut objects = header(CodingProfile::ObjectMetadata);
        objects.sound_bed_type = Some(SoundBedType::ObjectsOnly);
        objects.object_channel_number = Some(1); // semantic count = 2
        let plan = GaDecodePlan::from_header(&objects).unwrap();
        assert_eq!(plan.format, GaCodecFormat::Stereo);
        assert_eq!(plan.output_channels, Some(2));

        let mut mixed = header(CodingProfile::ObjectMetadata);
        mixed.sound_bed_type = Some(SoundBedType::ChannelBedAndObjects);
        mixed.channel_number_index = Some(2);
        mixed.channel_configuration = Some(ChannelConfiguration::Surround5_1);
        mixed.object_channel_number = Some(1); // 6-channel bed + 2 objects
        let plan = GaDecodePlan::from_header(&mixed).unwrap();
        assert_eq!(plan.format, GaCodecFormat::Multichannel);
        assert_eq!(plan.output_channels, Some(8));
    }

    #[test]
    fn routes_hoa_separately() {
        let mut hoa = header(CodingProfile::Hoa);
        hoa.hoa_order = Some(3);
        hoa.bitrate_index = Some(1);
        let plan = GaDecodePlan::from_header(&hoa).unwrap();
        assert_eq!(plan.format, GaCodecFormat::Hoa);
        assert_eq!(plan.output_channels, Some(16));
    }

    #[test]
    fn coded_payload_honors_header_boundary() {
        let mut frame = header(CodingProfile::Basic);
        frame.payload_offset_bytes = 3;
        assert_eq!(coded_payload(&[1, 2, 3, 4, 5], &frame).unwrap(), &[4, 5]);
        frame.payload_offset_bytes = 6;
        assert_eq!(
            coded_payload(&[1, 2, 3], &frame),
            Err(CodecError::Truncated)
        );
    }
}
