use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpatialError {
    InvalidSampleRate,
    InvalidBlockFrames,
    InvalidSourceCapacity,
    UnsupportedChannelLayout,
    ChannelCountMismatch,
    OutputTooSmall,
    SourceCapacityExceeded,
}

impl fmt::Display for SpatialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidSampleRate => "spatial engine sample rate must be non-zero",
            Self::InvalidBlockFrames => "spatial engine block size must be non-zero",
            Self::InvalidSourceCapacity => "spatial engine source capacity must be non-zero",
            Self::UnsupportedChannelLayout => "unsupported spatial channel layout",
            Self::ChannelCountMismatch => "input channel count does not match the selected layout",
            Self::OutputTooSmall => {
                "spatial output buffer is smaller than the required stereo output"
            }
            Self::SourceCapacityExceeded => {
                "spatial source count exceeds the preallocated capacity"
            }
        };
        formatter.write_str(message)
    }
}

impl Error for SpatialError {}
