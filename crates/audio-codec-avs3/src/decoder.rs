use yinqidao_codec_core::{
    AudioDecoder, AudioFrame, CodecError, CodecId, DecodeStatus, StreamInfo,
};

use crate::{
    BASE_OUTPUT_POSITIONS, Av3aSampleEntry, BasicMonoSynthesisWorkspace,
    BasicMultichannelSynthesisWorkspace, BasicStereoSynthesisWorkspace, BweConfig, BweMode,
    BweSideInfo, ChannelConfiguration, CoreSidePrefix, DynamicChannelPrefix, DynamicMetadata,
    DynamicMetadataPrefix, GaCodecFormat, GaDecodePlan, GaHoaFrameSideInfo,
    GaMultichannelFrameSideInfo, HoaConfig, HoaSynthesisWorkspace, NeuralNetworkType,
    StaticMetadataPrefix, TransformType,
    config::{AudioCodingMethod, Avs3SpecificConfig, ContentType, GeneralFullRateConfig, parse_dca3},
    dynamic_metadata::parse_dynamic_metadata_at,
    frame::{AatfFrameHeader, SoundBedType, parse_aatf_frame_header},
    ga::coded_payload,
    ga_mono_pcm::parse_decode_mono_pcm,
    ga_multichannel_pcm::parse_decode_multichannel_pcm,
    ga_stereo_pcm::parse_decode_stereo_pcm,
    hoa_synthesis::parse_decode_hoa_pcm,
    metadata::{MetadataBoundary, parse_metadata_boundary},
    metadata_prefix::parse_static_metadata_prefix_at,
};

#[derive(Clone, Copy, Debug, Default)]
struct ResolvedBwe {
    present: Option<bool>,
    config: Option<BweConfig>,
}

/// Incremental pure-Rust AVS3-P3 decoder state.
///
/// Basic and Low-Complexity mono/stereo/multichannel/HOA general-full-rate frames execute the
/// complete built-in path through neural inverse-QC and profile-specific reconstruction to PCM.
/// Stereo includes conventional M/S/ILD and <=32-kb/s MCR; multichannel applies MCAC/LFE handling;
/// HOA includes transport inverse-DMX, 512-hop analysis/synthesis and delayed spatial basis
/// recovery. Lossless coding and static metadata bodies remain explicit later milestones.
pub struct Avs3Decoder {
    info: StreamInfo,
    decoder_config: Vec<u8>,
    specific_config: Option<Avs3SpecificConfig>,
    mono_synthesis: BasicMonoSynthesisWorkspace,
    stereo_synthesis: BasicStereoSynthesisWorkspace,
    multichannel_synthesis: BasicMultichannelSynthesisWorkspace,
    hoa_synthesis: HoaSynthesisWorkspace,
    last_frame_header: Option<AatfFrameHeader>,
    last_decode_plan: Option<GaDecodePlan>,
    last_metadata_boundary: Option<MetadataBoundary>,
    last_static_metadata_prefix: Option<StaticMetadataPrefix>,
    last_dynamic_metadata_prefix: Option<DynamicMetadataPrefix>,
    last_dynamic_metadata: Option<DynamicMetadata>,
    /// Compatibility/diagnostic view of the first coded channel.
    last_core_side_prefix: Option<CoreSidePrefix>,
    last_first_transform_type: Option<TransformType>,
    last_bwe_present: Option<bool>,
    last_bwe_config: Option<BweConfig>,
    /// Compatibility/diagnostic view of the first coded channel's BWE data.
    last_bwe_side_info: Option<BweSideInfo>,
    last_after_bwe_bit_offset: Option<usize>,
    /// Full frame-major state for multichannel/object/mixed decoding.
    last_multichannel_frame_side_info: Option<GaMultichannelFrameSideInfo>,
    /// Full HOA transport/spatial side information for the most recently decoded HOA frame.
    last_hoa_frame_side_info: Option<GaHoaFrameSideInfo>,
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
            mono_synthesis: BasicMonoSynthesisWorkspace::new(),
            stereo_synthesis: BasicStereoSynthesisWorkspace::new(),
            multichannel_synthesis: BasicMultichannelSynthesisWorkspace::new(),
            hoa_synthesis: HoaSynthesisWorkspace::new(),
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
            last_multichannel_frame_side_info: None,
            last_hoa_frame_side_info: None,
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

    pub fn last_multichannel_frame_side_info(&self) -> Option<&GaMultichannelFrameSideInfo> {
        self.last_multichannel_frame_side_info.as_ref()
    }

    pub fn last_hoa_frame_side_info(&self) -> Option<&GaHoaFrameSideInfo> {
        self.last_hoa_frame_side_info.as_ref()
    }

    pub fn packets_seen(&self) -> u64 {
        self.packets_seen
    }

    fn general_config(&self) -> Result<&GeneralFullRateConfig, CodecError> {
        match self.specific_config.as_ref() {
            Some(Avs3SpecificConfig::GeneralFullRate(config)) => Ok(config),
            _ => Err(CodecError::InvalidData(
                "general-full-rate decoding requires dca3 GA configuration",
            )),
        }
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

    fn resolve_bwe_config(
        &self,
        header: &AatfFrameHeader,
        plan: GaDecodePlan,
    ) -> Result<ResolvedBwe, CodecError> {
        let config = self.general_config()?;
        let mode = match plan.format {
            GaCodecFormat::Mono => BweMode::Mono,
            GaCodecFormat::Stereo => BweMode::Stereo,
            GaCodecFormat::Multichannel => {
                let channels = plan.output_channels.ok_or(CodecError::InvalidData(
                    "multichannel BWE is missing output channel count",
                ))?;
                let non_lfe_channels = if lfe_channel_index(header.channel_configuration).is_some() {
                    channels.saturating_sub(1)
                } else {
                    channels
                };
                BweMode::Multichannel { non_lfe_channels }
            }
            // HOA BWE is resolved independently per transport group/channel from its bitrate table.
            GaCodecFormat::Hoa => return Ok(ResolvedBwe::default()),
        };

        let bwe_config = BweConfig::for_bitrate(mode, u32::from(config.total_bitrate_kbps))?;
        Ok(ResolvedBwe {
            present: Some(bwe_config.is_some()),
            config: bwe_config,
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
                ContentType::Hoa => {
                    if config.hoa_order != header.hoa_order {
                        return Err(CodecError::InvalidData(
                            "AATF HOA order does not match dca3 configuration",
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn clear_frame_diagnostics(&mut self) {
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
        self.last_multichannel_frame_side_info = None;
        self.last_hoa_frame_side_info = None;
    }
}

/// AVS/UWA channel-based layouts use the standard interleaved order L, R, C, LFE, ... .
/// Object-only streams have no channel-bed LFE; mixed streams keep bed channels first, so the LFE
/// remains index three before appended object signals.
fn lfe_channel_index(configuration: Option<ChannelConfiguration>) -> Option<usize> {
    match configuration {
        Some(
            ChannelConfiguration::Surround5_1
            | ChannelConfiguration::Surround7_1
            | ChannelConfiguration::Surround5_1_2
            | ChannelConfiguration::Surround5_1_4
            | ChannelConfiguration::Surround7_1_2
            | ChannelConfiguration::Surround7_1_4,
        ) => Some(3),
        _ => None,
    }
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

        self.clear_frame_diagnostics();
        let header = parse_aatf_frame_header(packet)?;
        self.validate_frame_against_config(&header)?;
        let method = header.coding_method;

        if method == AudioCodingMethod::Lossless {
            self.last_frame_header = Some(header);
            self.packets_seen = self.packets_seen.saturating_add(1);
            return Err(CodecError::Unsupported(
                "AVS3-P3 ll_raw_data_block lossless synthesis is not implemented yet",
            ));
        }

        let plan = GaDecodePlan::from_header(&header)?;
        let payload = coded_payload(packet, &header)?;
        if payload.is_empty() {
            return Err(CodecError::Truncated);
        }
        let metadata = parse_metadata_boundary(payload)?;
        self.last_decode_plan = Some(plan);
        self.last_metadata_boundary = Some(metadata);

        let core_bit_offset = match metadata {
            MetadataBoundary::None { core_bit_offset } => core_bit_offset,
            MetadataBoundary::StaticPresent { static_bit_offset } => {
                let prefix = parse_static_metadata_prefix_at(payload, static_bit_offset)?;
                if !prefix.basic_level_supported() {
                    return Err(CodecError::Unsupported(
                        "reserved AVS3 basic static metadata level",
                    ));
                }
                self.last_static_metadata_prefix = Some(prefix);
                self.last_frame_header = Some(header);
                self.packets_seen = self.packets_seen.saturating_add(1);
                return Err(CodecError::Unsupported(
                    "AVS3-P3 BasicL1/VrExt static metadata body decoding is not implemented yet",
                ));
            }
            MetadataBoundary::DynamicPresent { dynamic_bit_offset } => {
                let object_channels = header.object_channels().ok_or(CodecError::InvalidData(
                    "dynamic Audio Vivid metadata present without object channels",
                ))?;
                let decoded = parse_dynamic_metadata_at(payload, dynamic_bit_offset, object_channels)?;
                let first_channel_bit_offset = dynamic_bit_offset.saturating_add(3);
                let first_channel = decoded.objects.first().map(|object| DynamicChannelPrefix {
                    mute: object.mute,
                    transport_channel_ref: object.transport_channel_ref,
                    body_bit_offset: first_channel_bit_offset.saturating_add(6),
                });
                self.last_dynamic_metadata_prefix = Some(DynamicMetadataPrefix {
                    dm_level: decoded.level,
                    channel_count: object_channels,
                    first_channel_bit_offset,
                    first_channel,
                });
                let core_bit_offset = decoded.core_bit_offset;
                self.last_dynamic_metadata = Some(decoded);
                core_bit_offset
            }
        };

        let resolved_bwe = self.resolve_bwe_config(&header, plan)?;
        self.last_bwe_present = resolved_bwe.present;
        self.last_bwe_config = resolved_bwe.config;

        match plan.format {
            GaCodecFormat::Multichannel => {
                let nn_type = header.nn_type.ok_or(CodecError::InvalidData(
                    "general-full-rate AATF frame is missing neural-network type",
                ))?;
                if matches!(nn_type, NeuralNetworkType::Reserved(_)) {
                    return Err(CodecError::Unsupported(
                        "reserved AVS3 neural-network type",
                    ));
                }
                let low_bitrate = self
                    .lsf_low_bitrate_precision(plan)
                    .ok_or(CodecError::InvalidData(
                        "multichannel synthesis requires bitrate/channel configuration",
                    ))?;
                let channel_count = plan.output_channels.ok_or(CodecError::InvalidData(
                    "multichannel decode plan is missing channel count",
                ))?;
                if channel_count < 3 {
                    return Err(CodecError::InvalidData(
                        "multichannel decode plan contains fewer than three channels",
                    ));
                }
                let total_bitrate_kbps = u32::from(self.general_config()?.total_bitrate_kbps);
                let lfe_index = lfe_channel_index(header.channel_configuration);
                let sample_count = usize::from(channel_count)
                    .checked_mul(BASE_OUTPUT_POSITIONS)
                    .ok_or(CodecError::InvalidData(
                        "multichannel PCM output geometry overflows address space",
                    ))?;

                output.clear_for(
                    header.sample_rate.unwrap_or(self.info.sample_rate),
                    channel_count,
                );
                output.samples.resize(sample_count, 0.0);
                let frame = parse_decode_multichannel_pcm(
                    nn_type,
                    payload,
                    core_bit_offset,
                    low_bitrate,
                    resolved_bwe.config,
                    total_bitrate_kbps,
                    channel_count,
                    lfe_index,
                    &mut self.multichannel_synthesis,
                    &mut output.samples,
                )?;

                if let Some(first) = frame.channels.first() {
                    self.last_core_side_prefix = Some(first.core);
                    self.last_first_transform_type = Some(first.core.transform_type);
                    self.last_bwe_side_info = first.bwe;
                    self.last_after_bwe_bit_offset = Some(
                        first
                            .bwe
                            .map(|bwe| bwe.next_bit_offset)
                            .unwrap_or(first.core.next_bit_offset),
                    );
                }
                self.last_multichannel_frame_side_info = Some(frame);
                self.last_frame_header = Some(header);
                self.packets_seen = self.packets_seen.saturating_add(1);
                Ok(DecodeStatus::FrameReady)
            }
            GaCodecFormat::Mono => {
                let nn_type = header.nn_type.ok_or(CodecError::InvalidData(
                    "general-full-rate AATF frame is missing neural-network type",
                ))?;
                if matches!(nn_type, NeuralNetworkType::Reserved(_)) {
                    return Err(CodecError::Unsupported(
                        "reserved AVS3 neural-network type",
                    ));
                }
                let low_bitrate = self
                    .lsf_low_bitrate_precision(plan)
                    .ok_or(CodecError::InvalidData(
                        "mono synthesis requires bitrate/channel configuration",
                    ))?;

                output.clear_for(header.sample_rate.unwrap_or(self.info.sample_rate), 1);
                output.samples.resize(BASE_OUTPUT_POSITIONS, 0.0);
                let side = parse_decode_mono_pcm(
                    nn_type,
                    payload,
                    core_bit_offset,
                    low_bitrate,
                    resolved_bwe.config,
                    &mut self.mono_synthesis,
                    &mut output.samples,
                )?;
                let core = side.channel.core;
                self.last_core_side_prefix = Some(core);
                self.last_first_transform_type = Some(core.transform_type);
                self.last_bwe_side_info = side.channel.bwe;
                self.last_after_bwe_bit_offset = Some(
                    side.channel
                        .bwe
                        .map(|bwe| bwe.next_bit_offset)
                        .unwrap_or(core.next_bit_offset),
                );
                self.last_frame_header = Some(header);
                self.packets_seen = self.packets_seen.saturating_add(1);
                Ok(DecodeStatus::FrameReady)
            }
            GaCodecFormat::Stereo => {
                let nn_type = header.nn_type.ok_or(CodecError::InvalidData(
                    "general-full-rate AATF frame is missing neural-network type",
                ))?;
                if matches!(nn_type, NeuralNetworkType::Reserved(_)) {
                    return Err(CodecError::Unsupported(
                        "reserved AVS3 neural-network type",
                    ));
                }
                let low_bitrate = self
                    .lsf_low_bitrate_precision(plan)
                    .ok_or(CodecError::InvalidData(
                        "stereo synthesis requires bitrate/channel configuration",
                    ))?;
                let total_bitrate_kbps = u32::from(self.general_config()?.total_bitrate_kbps);

                output.clear_for(header.sample_rate.unwrap_or(self.info.sample_rate), 2);
                output.samples.resize(BASE_OUTPUT_POSITIONS * 2, 0.0);
                let side = parse_decode_stereo_pcm(
                    nn_type,
                    payload,
                    core_bit_offset,
                    low_bitrate,
                    resolved_bwe.config,
                    total_bitrate_kbps,
                    &mut self.stereo_synthesis,
                    &mut output.samples,
                )?;
                let first = &side.channels[0];
                self.last_core_side_prefix = Some(first.core);
                self.last_first_transform_type = Some(first.core.transform_type);
                self.last_bwe_side_info = first.bwe;
                self.last_after_bwe_bit_offset = Some(
                    first
                        .bwe
                        .map(|bwe| bwe.next_bit_offset)
                        .unwrap_or(first.core.next_bit_offset),
                );
                self.last_frame_header = Some(header);
                self.packets_seen = self.packets_seen.saturating_add(1);
                Ok(DecodeStatus::FrameReady)
            }
            GaCodecFormat::Hoa => {
                let nn_type = header.nn_type.ok_or(CodecError::InvalidData(
                    "general-full-rate HOA frame is missing neural-network type",
                ))?;
                if matches!(nn_type, NeuralNetworkType::Reserved(_)) {
                    return Err(CodecError::Unsupported(
                        "reserved AVS3 neural-network type",
                    ));
                }
                let order = header.hoa_order.ok_or(CodecError::InvalidData(
                    "HOA frame is missing semantic ambisonic order",
                ))?;
                let total_bitrate_kbps = u32::from(self.general_config()?.total_bitrate_kbps);
                let hoa_config = HoaConfig::for_order_bitrate(order, total_bitrate_kbps)?;
                let channel_count = u16::from(hoa_config.output_channels);
                if plan.output_channels != Some(channel_count) {
                    return Err(CodecError::InvalidData(
                        "HOA decode-plan channel count disagrees with bitrate configuration",
                    ));
                }
                let sample_count = usize::from(channel_count)
                    .checked_mul(BASE_OUTPUT_POSITIONS)
                    .ok_or(CodecError::InvalidData("HOA PCM output geometry overflow"))?;

                output.clear_for(
                    header.sample_rate.unwrap_or(self.info.sample_rate),
                    channel_count,
                );
                output.samples.resize(sample_count, 0.0);
                let frame = parse_decode_hoa_pcm(
                    nn_type,
                    payload,
                    core_bit_offset,
                    order,
                    total_bitrate_kbps,
                    &mut self.hoa_synthesis,
                    &mut output.samples,
                )?;

                if let Some(first) = frame.channels.first() {
                    self.last_core_side_prefix = Some(first.core);
                    self.last_first_transform_type = Some(first.core.transform_type);
                    self.last_bwe_side_info = first.bwe;
                    self.last_after_bwe_bit_offset = Some(
                        first
                            .bwe
                            .map(|bwe| bwe.next_bit_offset)
                            .unwrap_or(first.core.next_bit_offset),
                    );
                }
                self.last_bwe_present = Some(
                    frame
                        .channel_bwe_configs
                        .iter()
                        .any(Option::is_some),
                );
                self.last_bwe_config = frame
                    .channel_bwe_configs
                    .iter()
                    .copied()
                    .flatten()
                    .next();
                self.last_hoa_frame_side_info = Some(frame);
                self.last_frame_header = Some(header);
                self.packets_seen = self.packets_seen.saturating_add(1);
                Ok(DecodeStatus::FrameReady)
            }
        }
    }

    fn flush(&mut self, _output: &mut AudioFrame) -> Result<DecodeStatus, CodecError> {
        Ok(DecodeStatus::EndOfStream)
    }

    fn reset(&mut self) {
        self.last_frame_header = None;
        self.clear_frame_diagnostics();
        self.mono_synthesis.reset_synthesis_history();
        self.stereo_synthesis.reset_synthesis_history();
        self.multichannel_synthesis.reset_synthesis_history();
        self.hoa_synthesis.reset();
        self.packets_seen = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_standard_lfe_index_for_channel_beds() {
        assert_eq!(
            lfe_channel_index(Some(ChannelConfiguration::Surround7_1_4)),
            Some(3)
        );
        assert_eq!(
            lfe_channel_index(Some(ChannelConfiguration::Surround5_1)),
            Some(3)
        );
        assert_eq!(lfe_channel_index(Some(ChannelConfiguration::Stereo)), None);
        assert_eq!(lfe_channel_index(None), None);
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
        assert!(decoder.last_multichannel_frame_side_info().is_none());
        assert!(decoder.last_hoa_frame_side_info().is_none());
    }
}
