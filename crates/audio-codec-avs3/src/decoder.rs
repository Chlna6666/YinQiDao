use yinqidao_codec_core::{
    AudioDecoder, AudioFrame, CodecError, CodecId, DecodeStatus, StreamInfo,
};

use crate::{
    Av3aSampleEntry,
    config::{AudioCodingMethod, Avs3SpecificConfig, parse_dca3},
    frame::{AatfFrameHeader, parse_aatf_frame_header},
};

/// Incremental pure-Rust AVS3-P3 decoder state.
///
/// The container/config and normative AATF framing layers are implemented. The next milestone is
/// decoding `ga_co_raw_data_block()` / `ll_raw_data_block()` into spectral/PCM data; until then the
/// decoder deliberately returns `Unsupported` after validating and recording each real frame header.
pub struct Avs3Decoder {
    info: StreamInfo,
    decoder_config: Vec<u8>,
    specific_config: Option<Avs3SpecificConfig>,
    last_frame_header: Option<AatfFrameHeader>,
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

    pub fn packets_seen(&self) -> u64 {
        self.packets_seen
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
        Ok(())
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

        let header = parse_aatf_frame_header(packet)?;
        self.validate_frame_against_config(&header)?;
        self.packets_seen = self.packets_seen.saturating_add(1);
        output.clear_for(
            header.sample_rate.unwrap_or(self.info.sample_rate),
            header.resolved_channels().unwrap_or(self.info.channels),
        );

        let method = header.coding_method;
        self.last_frame_header = Some(header);
        match method {
            AudioCodingMethod::GeneralFullRate => Err(CodecError::Unsupported(
                "AVS3-P3 ga_co_raw_data_block entropy/transform synthesis is not implemented yet",
            )),
            AudioCodingMethod::Lossless => Err(CodecError::Unsupported(
                "AVS3-P3 ll_raw_data_block lossless synthesis is not implemented yet",
            )),
        }
    }

    fn flush(&mut self, _output: &mut AudioFrame) -> Result<DecodeStatus, CodecError> {
        Ok(DecodeStatus::EndOfStream)
    }

    fn reset(&mut self) {
        self.last_frame_header = None;
        self.packets_seen = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
