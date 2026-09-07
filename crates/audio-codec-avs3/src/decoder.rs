use yinqidao_codec_core::{
    AudioDecoder, AudioFrame, CodecError, CodecId, DecodeStatus, StreamInfo,
};

use crate::{Av3aSampleEntry, bitreader::BitReader};

/// Incremental AVS3-P3 decoder state.
///
/// Container/config parsing and the stable decoder API are implemented now. The normative AVS3-P3
/// spectral/entropy tools are intentionally not guessed from sample files; they will be filled in
/// from the published bitstream specification and conformance vectors in subsequent milestones.
pub struct Avs3Decoder {
    info: StreamInfo,
    decoder_config: Vec<u8>,
    packets_seen: u64,
}

impl Avs3Decoder {
    pub fn new(entry: &Av3aSampleEntry) -> Self {
        Self {
            info: StreamInfo {
                codec: CodecId::Avs3,
                sample_rate: entry.sample_rate.max(1),
                channels: entry.channels.max(1),
                bits_per_sample: entry.sample_size_bits.and_then(|bits| u8::try_from(bits).ok()),
                duration: None,
            },
            decoder_config: entry.decoder_config.clone(),
            packets_seen: 0,
        }
    }

    pub fn decoder_config(&self) -> &[u8] {
        &self.decoder_config
    }

    pub fn packets_seen(&self) -> u64 {
        self.packets_seen
    }

    fn validate_packet_shape(packet: &[u8]) -> Result<(), CodecError> {
        if packet.is_empty() {
            return Err(CodecError::Truncated);
        }

        // Exercise the shared MSB-first reader without assuming undocumented AVS3 syntax fields.
        // The actual frame parser will consume these bits once the normative syntax module lands.
        let mut reader = BitReader::new(packet);
        let _ = reader.read_bit()?;
        if reader.bits_remaining() > 0 {
            reader.skip_bits(reader.bits_remaining().min(7))?;
            reader.align_byte();
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
        Self::validate_packet_shape(packet)?;
        self.packets_seen = self.packets_seen.saturating_add(1);
        output.clear_for(self.info.sample_rate, self.info.channels);

        Err(CodecError::Unsupported(
            "AVS3-P3 entropy/transform synthesis is not implemented yet",
        ))
    }

    fn flush(&mut self, _output: &mut AudioFrame) -> Result<DecodeStatus, CodecError> {
        Ok(DecodeStatus::EndOfStream)
    }

    fn reset(&mut self) {
        self.packets_seen = 0;
    }
}
