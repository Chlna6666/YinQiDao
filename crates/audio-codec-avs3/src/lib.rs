//! Pure-Rust AVS3-P3 / AV3A codec work for YinQiDao.
//!
//! The crate is deliberately split from the player so container parsing, bitstream syntax,
//! synthesis and future object/HOA rendering can evolve independently and be tested on desktop
//! and mobile targets. No FFmpeg process or native codec DLL is required by this crate.

mod bitreader;
mod config;
mod container;
mod decoder;
mod frame;
mod synthesis;

pub use config::{
    AudioCodingMethod, Avs3SpecificConfig, ChannelConfiguration, CodingProfile, ContentType,
    GeneralFullRateConfig, LosslessConfig, NeuralNetworkType, QuantizationResolution,
    full_rate_sample_rate, parse_dca3,
};
pub use container::{Av3aSampleEntry, probe_av3a_bytes, probe_av3a_path};
pub use decoder::Avs3Decoder;
pub use frame::{AATF_SYNCWORD, AatfFrameHeader, SoundBedType, parse_aatf_frame_header};
pub use synthesis::{
    apply_window_in_place, overlap_add, scale_pcm_in_place, spectral_dot,
};
