use std::{
    error::Error,
    fmt,
    io,
    time::Duration,
};

use yinqidao_codec_avs3::{Av3aIsoBmffDemuxer, Avs3Decoder};
use yinqidao_codec_core::{AudioDecoder, AudioFrame, CodecError, DecodeStatus};

#[derive(Debug)]
pub(crate) enum Av3aRustError {
    Io(io::Error),
    Codec(CodecError),
    UnexpectedStatus,
    InvalidMonoFrame,
}

impl fmt::Display for Av3aRustError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "AV3A ISO-BMFF 读取失败: {error}"),
            Self::Codec(error) => write!(formatter, "AVS3-P3 解码失败: {error}"),
            Self::UnexpectedStatus => formatter.write_str("AVS3-P3 完整 sample 未产生 PCM frame"),
            Self::InvalidMonoFrame => formatter.write_str("AVS3-P3 Basic mono 输出几何不合法"),
        }
    }
}

impl Error for Av3aRustError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Codec(error) => Some(error),
            Self::UnexpectedStatus | Self::InvalidMonoFrame => None,
        }
    }
}

impl From<io::Error> for Av3aRustError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<CodecError> for Av3aRustError {
    fn from(error: CodecError) -> Self {
        Self::Codec(error)
    }
}

/// Player-side pure-Rust AV3A backend for the currently complete Basic-profile mono path.
///
/// The first compressed sample is decoded once during capability probing and retained as the
/// first output frame. This avoids decoding frame zero twice while still allowing unsupported
/// AVS3 profiles/layouts to fall back to the transitional process backend before playback starts.
pub(crate) struct Av3aRustBackend {
    demuxer: Av3aIsoBmffDemuxer,
    decoder: Avs3Decoder,
    packet: Vec<u8>,
    frame: AudioFrame,
    first_frame_ready: bool,
    sample_rate: u32,
    channels: u16,
    sample_count: usize,
    duration: Duration,
}

impl Av3aRustBackend {
    pub(crate) fn from_demuxer(
        mut demuxer: Av3aIsoBmffDemuxer,
    ) -> Result<Option<Self>, Av3aRustError> {
        let entry = demuxer.sample_entry().clone();
        if entry.channels != 1 || entry.decoder_config.is_empty() {
            return Ok(None);
        }

        let sample_count = demuxer.sample_count();
        if sample_count == 0 {
            return Ok(None);
        }

        let duration = demuxer.duration();
        let mut decoder = Avs3Decoder::new(&entry)?;
        let mut packet = Vec::new();
        let Some(_) = demuxer.next_sample_into(&mut packet)? else {
            return Ok(None);
        };

        let mut frame = AudioFrame::default();
        match decoder.decode_packet(&packet, &mut frame) {
            Ok(DecodeStatus::FrameReady) => {}
            Ok(DecodeStatus::NeedMoreData | DecodeStatus::EndOfStream) => {
                return Err(Av3aRustError::UnexpectedStatus);
            }
            Err(CodecError::Unsupported(_)) => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        if frame.channels != 1 || frame.samples.len() != 1024 {
            return Err(Av3aRustError::InvalidMonoFrame);
        }

        let sample_rate = frame.sample_rate.max(1);
        Ok(Some(Self {
            demuxer,
            decoder,
            packet,
            frame,
            first_frame_ready: true,
            sample_rate,
            channels: 1,
            sample_count,
            duration,
        }))
    }

    pub(crate) fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub(crate) fn channels(&self) -> u16 {
        self.channels
    }

    pub(crate) fn sample_count(&self) -> usize {
        self.sample_count
    }

    pub(crate) fn duration(&self) -> Duration {
        self.duration
    }

    pub(crate) fn next_chunk_into(
        &mut self,
        samples: &mut Vec<f32>,
    ) -> Result<bool, Av3aRustError> {
        if self.first_frame_ready {
            self.first_frame_ready = false;
            std::mem::swap(samples, &mut self.frame.samples);
            return Ok(true);
        }

        loop {
            let Some(_) = self.demuxer.next_sample_into(&mut self.packet)? else {
                samples.clear();
                return Ok(false);
            };
            match self.decoder.decode_packet(&self.packet, &mut self.frame)? {
                DecodeStatus::FrameReady => {
                    if self.frame.channels != 1 || self.frame.samples.len() != 1024 {
                        return Err(Av3aRustError::InvalidMonoFrame);
                    }
                    std::mem::swap(samples, &mut self.frame.samples);
                    return Ok(true);
                }
                DecodeStatus::NeedMoreData => continue,
                DecodeStatus::EndOfStream => {
                    samples.clear();
                    return Ok(false);
                }
            }
        }
    }

    /// Reset all codec history and select the independently decodable sample containing `position`.
    pub(crate) fn seek(&mut self, position: Duration) -> Duration {
        self.demuxer.seek(position);
        self.decoder.reset();
        self.frame.samples.clear();
        self.first_frame_ready = false;
        self.demuxer.position()
    }
}
