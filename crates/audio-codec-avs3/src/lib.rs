//! Pure-Rust AVS3-P3 / AV3A codec work for YinQiDao.
//!
//! The crate is deliberately split from the player so container parsing, bitstream syntax,
//! synthesis and future object/HOA rendering can evolve independently and be tested on desktop
//! and mobile targets. No FFmpeg process or native codec DLL is required by this crate.

mod bitreader;
mod config;
mod container;
mod core;
mod decoder;
mod dynamic_metadata;
mod frame;
mod ga;
mod metadata;
mod metadata_prefix;
mod synthesis;
mod tns;

pub use config::{
    AudioCodingMethod, Avs3SpecificConfig, ChannelConfiguration, CodingProfile, ContentType,
    GeneralFullRateConfig, LosslessConfig, NeuralNetworkType, QuantizationResolution,
    full_rate_sample_rate, parse_dca3,
};
pub use container::{Av3aSampleEntry, probe_av3a_bytes, probe_av3a_path};
pub use core::{
    CoreSidePrefix, FdShapingSideInfo, TnsSideBoundary, TransformType,
    parse_core_side_prefix_at, parse_core_transform_type, parse_core_transform_type_at,
    parse_fd_shaping_at, parse_tns_boundary_at,
};
pub use decoder::Avs3Decoder;
pub use dynamic_metadata::{
    CartesianExtent, ChannelLock, DynamicLevel1, DynamicLevel2, DynamicMetadata,
    DynamicObjectMetadata, ObjectDivergence, ObjectPosition, PolarExtent,
    parse_dynamic_metadata_at,
};
pub use frame::{AATF_SYNCWORD, AatfFrameHeader, SoundBedType, parse_aatf_frame_header};
pub use ga::{GaCodecFormat, GaDecodePlan, coded_payload};
pub use metadata::{MetadataBoundary, parse_metadata_boundary};
pub use metadata_prefix::{
    DynamicChannelPrefix, DynamicMetadataPrefix, StaticMetadataPrefix,
    parse_dynamic_channel_prefix_at, parse_dynamic_metadata_prefix_at,
    parse_static_metadata_prefix_at,
};
pub use synthesis::{
    apply_window_in_place, overlap_add, scale_pcm_in_place, spectral_dot,
};
pub use tns::{
    TNS_REFLECTION_COEFFICIENTS, TnsFilterSideInfo, TnsSideInfo,
    parse_tns_side_info_at, reflection_coefficient,
};
