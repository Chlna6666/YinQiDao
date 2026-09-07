use yinqidao_codec_core::{
    AudioDecoder, AudioFrame, CodecError, CodecId, DecodeStatus, StreamInfo,
};

use crate::{
    Av3aSampleEntry, BweConfig, BweMode, BweSideInfo, ChannelConfiguration, CoreSidePrefix,
    DynamicChannelPrefix, DynamicMetadata, DynamicMetadataPrefix, StaticMetadataPrefix,
    TransformType,
    config::{AudioCodingMethod, Avs3SpecificConfig, ContentType, parse_dca3},
    core::{parse_core_side_prefix_at, parse_core_transform_type_at},
    dynamic_metadata::parse_dynamic_metadata_at,
    frame::{AatfFrameHeader, SoundBedType, parse_aatf_frame_header},
    ga::{GaCodecFormat, GaDecodePlan, coded_payload},
    metadata::{MetadataBoundary, parse_metadata_boundary},
    metadata_prefix::parse_static_metadata_prefix_at,
};

#[derive(Clone, Copy, Debug, Default)]
struct ParsedBwe {
    /// `Some(false)` means normatively disabled by bitrate, `Some(true)` means side information was
    /// consumed, and `None` means the current mode has not yet been resolved (currently HOA).
    present: Option<bool>,
    config: Option<BweConfig>,
    side_info: Option<BweSideInfo>,
    next_bit_offset: Option<usize>,
}

/// Incremental pure-Rust AVS3-P3 decoder state.
///
/// Container/config, AATF framing, full-rate routing, dynamic Audio Vivid L1/L2 metadata,
/// FdShaping, complete TNS Huffman data and mono/stereo/multichannel BWE side information are
/// parsed without native decoders. Static `BasicL1()`, HOA BWE, Group/Stereo/MC/QC, range decoding
/// and inverse quantization remain the next normative boundaries before PCM synthesis.
pub struct Avs3Decoder {
    info: StreamInfo,
    decoder_config: Vec<u8>,
    specific_config: Option<Avs3SpecificConfig>,
    last_frame_header: Option<AatfFrameHeader>,
    last_decode_plan: Option<GaDecodePlan>,
    last_metadata_boundary: Option<MetadataBoundary>,
    last_static_metadata_prefix: Option<StaticMetadataPrefix>,
    last_dynamic_metadata_prefix: Option<DynamicMetadataPrefix>,
    last_dynamic_metadata: Option<DynamicMetadata>,
    last_core_side_prefix: Option<CoreSidePrefix>,
    last_first_transform_type: Option<TransformType>,
    last_bwe_present: Option<bool>,
    last_bwe_config: Option<BweConfig>,
    last_bwe_side_info: Option<BweSideInfo>,
    last_after_bwe_bit_offset: Option<usize>,
    packets_seen: u64,
}

impl Avs3Decoder {
    pub fn new(entry: &Av3aSampleEntry) -> Result<Self, CodecError> {
        let specific_config = if entry.decoder_config.is_empty() {
            None
        } else {
            Some(parse_dca3(&entry.decoder_config)?)
        };
        let bits_per_sample = specific_config
            .as_ref()
            .and_then(Avs3SpecificConfig::bits_per_sample)
            .or_else(|| entry.sample_size_bits.and_then(|bits| u8::try_from(bits).ok()));
        let sample_rate = specific_config
            .as_ref()
            .and_then(Avs3SpecificConfig::sample_rate)
            .unwrap_or(entry.sample_rate.max(1));

        Ok(Self {
            info: StreamInfo {
                codec: CodecId::Avs3,
                sample_rate,
                channels: entry.channels.max(1),
                bits_per_sample,
                duration: None,
            },
            decoder_config: entry.decoder_config.clone(),
            specific_config,
            last_frame_header: None,
            last_decode_plan: None,
            last_metadata_boundary: None,
            last_static_metadata_prefix: None,
            last_dynamic_metadata_prefix: None,
            last_dynamic_metadata: None,
            last_core_side_prefix: None,
            last_first_transform_type: None,
            last_bwe_present: None,
            last_bwe_config: None,
            last_bwe_side_info: None,
            last_after_bwe_bit_offset: None,
            packets_seen: 0,
        })
    }

    pub fn decoder_config(&self) -> &[u8] {
        &self.decoder_config
    }

    pub fn specific_config(&self) -> Option<&Avs3SpecificConfig> {
        self.specific_config.as_ref()
    }

    pub fn last_frame_header(&self) -> Option<&AatfFrameHeader> {
        self.last_frame_header.as_ref()
    }

    pub fn last_decode_plan(&self) -> Option<GaDecodePlan> {
        self.last_decode_plan
    }

    pub fn last_metadata_boundary(&self) -> Option<MetadataBoundary> {
        self.last_metadata_boundary
    }

    pub fn last_static_metadata_prefix(&self) -> Option<StaticMetadataPrefix> {
        self.last_static_metadata_prefix
    }

    pub fn last_dynamic_metadata_prefix(&self) -> Option<DynamicMetadataPrefix> {
        self.last_dynamic_metadata_prefix
    }

    pub fn last_dynamic_metadata(&self) -> Option<&DynamicMetadata> {
        self.last_dynamic_metadata.as_ref()
    }

    pub fn last_core_side_prefix(&self) -> Option<CoreSidePrefix> {
        self.last_core_side_prefix
    }

    pub fn last_first_transform_type(&self) -> Option<TransformType> {
        self.last_first_transform_type
    }

    pub fn last_bwe_present(&self) -> Option<bool> {
        self.last_bwe_present
    }

    pub fn last_bwe_config(&self) -> Option<BweConfig> {
        self.last_bwe_config
    }

    pub fn last_bwe_side_info(&self) -> Option<BweSideInfo> {
        self.last_bwe_side_info
    }

    pub fn last_after_bwe_bit_offset(&self) -> Option<usize> {
        self.last_after_bwe_bit_offset
    }

    pub fn packets_seen(&self) -> u64 {
        self.packets_seen
    }

    fn lsf_low_bitrate_precision(&self, plan: GaDecodePlan) -> Option<bool> {
        let Avs3SpecificConfig::GeneralFullRate(config) = self.specific_config.as_ref()? else {
            return None;
        };
        let channels = u32::from(plan.output_channels?);
        if channels == 0 {
            return None;
        }
        Some(u32::from(config.total_bitrate_kbps) <= channels.saturating_mul(32))
    }

    fn parse_core_prefix(
        &self,
        payload: &[u8],
        core_bit_offset: usize,
        plan: GaDecodePlan,
    ) -> Result<(TransformType, Option<CoreSidePrefix>), CodecError> {
        let transform = parse_core_transform_type_at(payload, core_bit_offset)?;
        let prefix = self
            .lsf_low_bitrate_precision(plan)
            .map(|low_bitrate| parse_core_side_prefix_at(payload, core_bit_offset, low_bitrate))
            .transpose()?;
        Ok((transform, prefix))
    }

    fn parse_bwe_after_core(
        &self,
        payload: &[u8],
        header: &AatfFrameHeader,
        plan: GaDecodePlan,
        core: CoreSidePrefix,
    ) -> Result<ParsedBwe, CodecError> {
        let Avs3SpecificConfig::GeneralFullRate(config) = self.specific_config.as_ref().ok_or(
            CodecError::InvalidData("BWE resolution requires dca3 general-full-rate configuration"),
        )? else {
            return Ok(ParsedBwe::default());
        };

        let mode = match plan.format {
            GaCodecFormat::Mono => BweMode::Mono,
            GaCodecFormat::Stereo => BweMode::Stereo,
            GaCodecFormat::Multichannel => {
                let channels = plan.output_channels.ok_or(CodecError::InvalidData(
                    "multichannel BWE is missing output channel count",
                ))?;
                let non_lfe_channels = if channel_configuration_has_lfe(header.channel_configuration)
                {
                    channels.saturating_sub(1)
                } else {
                    channels
                };
                BweMode::Multichannel { non_lfe_channels }
            }
            // HOA uses its own tables 31/35/39/43 and is intentionally not treated as ordinary
            // multichannel audio; returning unresolved prevents silently consuming the wrong bits.
            GaCodecFormat::Hoa => return Ok(ParsedBwe::default()),
        };

        let Some(bwe_config) = BweConfig::for_bitrate(mode, u32::from(config.total_bitrate_kbps))?
        else {
            return Ok(ParsedBwe {
                present: Some(false),
                config: None,
                side_info: None,
                next_bit_offset: Some(core.next_bit_offset),
            });
        };

        let side_info = bwe_config.parse_side_info(payload, core.next_bit_offset)?;
        Ok(ParsedBwe {
            present: Some(true),
            config: Some(bwe_config),
            side_info: Some(side_info),
            next_bit_offset: Some(side_info.next_bit_offset),
        })
    }

    fn validate_frame_against_config(&self, header: &AatfFrameHeader) -> Result<(), CodecError> {
        let Some(config) = self.specific_config.as_ref() else {
            return Ok(());
        };
        if config.coding_method() != header.coding_method {
            return Err(CodecError::InvalidData(
                "AATF audio_codec_id does not match dca3 configuration",
            ));
        }
        if let (Some(config_rate), Some(frame_rate)) = (config.sample_rate(), header.sample_rate)
            && config_rate != frame_rate
        {
            return Err(CodecError::InvalidData(
                "AATF sampling frequency does not match dca3 configuration",
            ));
        }

        if let Avs3SpecificConfig::GeneralFullRate(config) = config {
            if config.content_type.coding_profile() != header.coding_profile {
                return Err(CodecError::InvalidData(
                    "AATF coding_profile does not match dca3 content_type",
                ));
            }

            match config.content_type {
                ContentType::Channel => {
                    if config.channel_number_index != header.channel_number_index {
                        return Err(CodecError::InvalidData(
                            "AATF channel_number_index does not match dca3 configuration",
                        ));
                    }
                }
                ContentType::Object => {
                    if header.sound_bed_type != Some(SoundBedType::ObjectsOnly) {
                        return Err(CodecError::InvalidData(
                            "AATF soundBedType does not match object-only dca3 configuration",
                        ));
                    }
                    if config.number_objects.map(u16::from) != header.object_channels() {
                        return Err(CodecError::InvalidData(
                            "AATF object count does not match dca3 configuration",
                        ));
                    }
                }
                ContentType::Mixed => {
                    if header.sound_bed_type != Some(SoundBedType::ChannelBedAndObjects) {
                        return Err(CodecError::InvalidData(
                            "AATF soundBedType does not match mixed dca3 configuration",
                        ));
                    }
                    if config.channel_number_index != header.channel_number_index {
                        return Err(CodecError::InvalidData(
                            "AATF mixed channel_number_index does not match dca3 configuration",
                        ));
                    }
                    if config.number_objects.map(u16::from) != header.object_channels() {
                        return Err(CodecError::InvalidData(
                            "AATF mixed object count does not match dca3 configuration",
                        ));
                    }
                }
                ContentType::Hoa => {}
            }
        }
        Ok(())
    }
}

fn channel_configuration_has_lfe(configuration: Option<ChannelConfiguration>) -> bool {
    matches!(
        configuration,
        Some(
            ChannelConfiguration::Surround5_1
                | ChannelConfiguration::Surround7_1
                | ChannelConfiguration::Surround5_1_2
                | ChannelConfiguration::Surround5_1_4
                | ChannelConfiguration::Surround7_1_2
                | ChannelConfiguration::Surround7_1_4
        )
    )
}

impl AudioDecoder for Avs3Decoder {
    fn codec_id(&self) -> CodecId {
        CodecId::Avs3
    }

    fn stream_info(&self) -> &StreamInfo {
        &self.info
    }

    fn decode_packet(
        &mut self,
        packet: &[u8],
        output: &mut AudioFrame,
    ) -> Result<DecodeStatus, CodecError> {
        if packet.is_empty() {
            return Ok(DecodeStatus::NeedMoreData);
        }

        let header = parse_aatf_frame_header(packet)?;
        self.validate_frame_against_config(&header)?;
        let method = header.coding_method;

        let (
            decode_plan,
            metadata_boundary,
            static_metadata_prefix,
            dynamic_metadata_prefix,
            dynamic_metadata,
            core_side_prefix,
            first_transform_type,
            parsed_bwe,
        ) = if method == AudioCodingMethod::GeneralFullRate {
            let plan = GaDecodePlan::from_header(&header)?;
            let payload = coded_payload(packet, &header)?;
            if payload.is_empty() {
                return Err(CodecError::Truncated);
            }
            let metadata = parse_metadata_boundary(payload)?;

            match metadata {
                MetadataBoundary::None { core_bit_offset } => {
                    let (transform, core) = self.parse_core_prefix(payload, core_bit_offset, plan)?;
                    let bwe = core
                        .map(|core| self.parse_bwe_after_core(payload, &header, plan, core))
                        .transpose()?
                        .unwrap_or_default();
                    (
                        Some(plan),
                        Some(metadata),
                        None,
                        None,
                        None,
                        core,
                        Some(transform),
                        bwe,
                    )
                }
                MetadataBoundary::StaticPresent { static_bit_offset } => {
                    let prefix = parse_static_metadata_prefix_at(payload, static_bit_offset)?;
                    if !prefix.basic_level_supported() {
                        return Err(CodecError::Unsupported(
                            "reserved AVS3 basic static metadata level",
                        ));
                    }
                    (
                        Some(plan),
                        Some(metadata),
                        Some(prefix),
                        None,
                        None,
                        None,
                        None,
                        ParsedBwe::default(),
                    )
                }
                MetadataBoundary::DynamicPresent { dynamic_bit_offset } => {
                    let object_channels = header.object_channels().ok_or(CodecError::InvalidData(
                        "dynamic Audio Vivid metadata present without object channels",
                    ))?;
                    let decoded = parse_dynamic_metadata_at(
                        payload,
                        dynamic_bit_offset,
                        object_channels,
                    )?;
                    let first_channel_bit_offset = dynamic_bit_offset.saturating_add(3);
                    let first_channel = decoded.objects.first().map(|object| DynamicChannelPrefix {
                        mute: object.mute,
                        transport_channel_ref: object.transport_channel_ref,
                        body_bit_offset: first_channel_bit_offset.saturating_add(6),
                    });
                    let prefix = DynamicMetadataPrefix {
                        dm_level: decoded.level,
                        channel_count: object_channels,
                        first_channel_bit_offset,
                        first_channel,
                    };
                    let (transform, core) =
                        self.parse_core_prefix(payload, decoded.core_bit_offset, plan)?;
                    let bwe = core
                        .map(|core| self.parse_bwe_after_core(payload, &header, plan, core))
                        .transpose()?
                        .unwrap_or_default();
                    (
                        Some(plan),
                        Some(metadata),
                        None,
                        Some(prefix),
                        Some(decoded),
                        core,
                        Some(transform),
                        bwe,
                    )
                }
            }
        } else {
            (
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                ParsedBwe::default(),
            )
        };

        self.packets_seen = self.packets_seen.saturating_add(1);
        output.clear_for(
            header.sample_rate.unwrap_or(self.info.sample_rate),
            decode_plan
                .and_then(|plan| plan.output_channels)
                .or_else(|| header.resolved_channels())
                .unwrap_or(self.info.channels),
        );
        self.last_decode_plan = decode_plan;
        self.last_metadata_boundary = metadata_boundary;
        self.last_static_metadata_prefix = static_metadata_prefix;
        self.last_dynamic_metadata_prefix = dynamic_metadata_prefix;
        self.last_dynamic_metadata = dynamic_metadata;
        self.last_core_side_prefix = core_side_prefix;
        self.last_first_transform_type = first_transform_type;
        self.last_bwe_present = parsed_bwe.present;
        self.last_bwe_config = parsed_bwe.config;
        self.last_bwe_side_info = parsed_bwe.side_info;
        self.last_after_bwe_bit_offset = parsed_bwe.next_bit_offset;
        self.last_frame_header = Some(header);

        match (method, metadata_boundary) {
            (
                AudioCodingMethod::GeneralFullRate,
                Some(MetadataBoundary::StaticPresent { .. }),
            ) => Err(CodecError::Unsupported(
                "AVS3-P3 BasicL1/VrExt static metadata body decoding is not implemented yet",
            )),
            (AudioCodingMethod::GeneralFullRate, _) if self.last_bwe_present.is_none() => {
                Err(CodecError::Unsupported(
                    "AVS3-P3 HOA BWE configuration is not implemented yet",
                ))
            }
            (AudioCodingMethod::GeneralFullRate, _) => Err(CodecError::Unsupported(
                "AVS3-P3 group/stereo-or-multichannel/QC and entropy synthesis is not implemented yet",
            )),
            (AudioCodingMethod::Lossless, _) => Err(CodecError::Unsupported(
                "AVS3-P3 ll_raw_data_block lossless synthesis is not implemented yet",
            )),
        }
    }

    fn flush(&mut self, _output: &mut AudioFrame) -> Result<DecodeStatus, CodecError> {
        Ok(DecodeStatus::EndOfStream)
    }

    fn reset(&mut self) {
        self.last_frame_header = None;
        self.last_decode_plan = None;
        self.last_metadata_boundary = None;
        self.last_static_metadata_prefix = None;
        self.last_dynamic_metadata_prefix = None;
        self.last_dynamic_metadata = None;
        self.last_core_side_prefix = None;
        self.last_first_transform_type = None;
        self.last_bwe_present = None;
        self.last_bwe_config = None;
        self.last_bwe_side_info = None;
        self.last_after_bwe_bit_offset = None;
        self.packets_seen = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_lfe_only_for_bed_layouts_that_define_one() {
        assert!(channel_configuration_has_lfe(Some(
            ChannelConfiguration::Surround7_1_4
        )));
        assert!(!channel_configuration_has_lfe(Some(
            ChannelConfiguration::Stereo
        )));
        assert!(!channel_configuration_has_lfe(None));
    }

    #[test]
    fn rejects_non_aatf_packet_before_decode_stage() {
        let entry = Av3aSampleEntry {
            sample_rate: 48_000,
            channels: 2,
            sample_size_bits: Some(24),
            decoder_config: Vec::new(),
        };
        let mut decoder = Avs3Decoder::new(&entry).unwrap();
        let mut output = AudioFrame::default();
        assert_eq!(
            decoder.decode_packet(&[0; 8], &mut output),
            Err(CodecError::InvalidData("invalid AATF syncword"))
        );
        assert_eq!(decoder.packets_seen(), 0);
        assert_eq!(decoder.last_decode_plan(), None);
        assert_eq!(decoder.last_metadata_boundary(), None);
        assert_eq!(decoder.last_static_metadata_prefix(), None);
        assert_eq!(decoder.last_dynamic_metadata_prefix(), None);
        assert!(decoder.last_dynamic_metadata().is_none());
        assert_eq!(decoder.last_core_side_prefix(), None);
        assert_eq!(decoder.last_bwe_present(), None);
    }
}
