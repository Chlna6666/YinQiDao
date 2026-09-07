use std::{error::Error, fmt, time::Duration};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum CodecId {
    Avs3,
    Ape,
    WavPack,
    Opus,
    Musepack,
    Ac3,
    Eac3,
    Dts,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamInfo {
    pub codec: CodecId,
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: Option<u8>,
    pub duration: Option<Duration>,
}

impl StreamInfo {
    pub fn new(codec: CodecId, sample_rate: u32, channels: u16) -> Self {
        Self {
            codec,
            sample_rate: sample_rate.max(1),
            channels: channels.max(1),
            bits_per_sample: None,
            duration: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct AudioFrame {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    pub pts: Option<Duration>,
}

impl AudioFrame {
    pub fn clear_for(&mut self, sample_rate: u32, channels: u16) {
        self.samples.clear();
        self.sample_rate = sample_rate.max(1);
        self.channels = channels.max(1);
        self.pts = None;
    }

    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeStatus {
    FrameReady,
    NeedMoreData,
    EndOfStream,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum CodecError {
    InvalidData(&'static str),
    Unsupported(&'static str),
    Truncated,
    Internal(String),
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidData(message) => write!(f, "invalid codec data: {message}"),
            Self::Unsupported(message) => write!(f, "unsupported codec feature: {message}"),
            Self::Truncated => f.write_str("truncated codec packet"),
            Self::Internal(message) => write!(f, "codec internal error: {message}"),
        }
    }
}

impl Error for CodecError {}

pub trait AudioDecoder: Send {
    fn codec_id(&self) -> CodecId;
    fn stream_info(&self) -> &StreamInfo;
    fn decode_packet(
        &mut self,
        packet: &[u8],
        output: &mut AudioFrame,
    ) -> Result<DecodeStatus, CodecError>;
    fn flush(&mut self, output: &mut AudioFrame) -> Result<DecodeStatus, CodecError>;
    fn reset(&mut self);
}
