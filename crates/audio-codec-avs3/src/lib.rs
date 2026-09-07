//! Pure-Rust AVS3-P3 / AV3A codec work for YinQiDao.
//!
//! The crate is deliberately split from the player so container parsing, bitstream syntax,
//! entropy/range decoding, neural reconstruction, synthesis and future object/HOA rendering can
//! evolve independently and be tested on desktop and mobile targets. No FFmpeg process or native
//! codec DLL is required.

mod allocation;
mod base_b10;
mod base_neural;
mod base_normative;
mod base_params;
mod base_pipeline;
mod bitreader;
mod bwe;
mod bwe_synthesis;
mod config;
mod container;
mod context_params;
mod context_pipeline;
mod core;
mod decoder;
mod dynamic_metadata;
mod entropy;
mod fd_lsf;
mod fd_lsf_tables;
mod fd_shaping;
mod frame;
mod ga;
mod ga_frame;
mod ga_mono;
mod ga_mono_neural;
mod ga_mono_pcm;
mod ga_mono_post;
mod ga_stereo;
mod ga_stereo_pcm;
mod group;
mod imdct_synthesis;
mod inverse_qc;
mod isobmff;
mod metadata;
mod metadata_prefix;
mod multichannel;
mod neural;
mod qc;
mod quantizer_params;
mod range;
mod range_tables;
mod stereo_synthesis;
mod synthesis;
mod tns;
mod tns_synthesis;

pub use allocation::{McBitAllocation, allocate_multichannel_bytes, lfe_allocation_bytes};
pub use base_neural::{
    BASE_INPUT_CHANNELS, BASE_INPUT_POSITIONS, BASE_LAYER_1_SPEC, BASE_LAYER_2_SPEC,
    BASE_LAYER_3_SPEC, BASE_LAYER_4_SPEC, BASE_OUTPUT_POSITIONS, BaseDecoderParams,
    BaseDecoderWorkspace, IgdnParams, apply_igdn_in_place, decode_base_network,
};
pub use base_normative::decode_basic_base_to_mdct_normative;
pub use base_params::{
    BASE_LAYER_1_BIAS, BASE_LAYER_1_IGDN_BETA, BASE_LAYER_1_IGDN_GAMMA,
    BASE_LAYER_1_KERNEL, BASE_LAYER_2_BIAS, BASE_LAYER_2_IGDN_BETA,
    BASE_LAYER_2_IGDN_GAMMA, BASE_LAYER_2_KERNEL, BASE_LAYER_3_BIAS,
    BASE_LAYER_3_IGDN_BETA, BASE_LAYER_3_IGDN_GAMMA, BASE_LAYER_3_KERNEL,
    BASE_LAYER_4_BIAS, BASE_LAYER_4_KERNEL, base_decoder_params,
};
pub use base_pipeline::{
    BasePipelineWorkspace, NoiseFillingRng, apply_base_noise_filling_in_place,
    decode_basic_base_to_mdct, dequantize_base_latents_into,
};
pub use bwe::{BweConfig, BweMode, BweSideInfo, WhiteningLevel, parse_bwe_side_info_at};
pub use bwe_synthesis::{BweSynthesisWorkspace, BweWhiteningRng, apply_bwe_synthesis};
pub use config::{
    AudioCodingMethod, Avs3SpecificConfig, ChannelConfiguration, CodingProfile, ContentType,
    GeneralFullRateConfig, LosslessConfig, NeuralNetworkType, QuantizationResolution,
    full_rate_sample_rate, parse_dca3,
};
pub use container::{Av3aSampleEntry, probe_av3a_bytes, probe_av3a_path};
pub use context_params::{
    CONTEXT_LAYER_1_BIAS, CONTEXT_LAYER_1_KERNEL, CONTEXT_LAYER_2_BIAS,
    CONTEXT_LAYER_2_KERNEL, CONTEXT_LAYER_3_BIAS, CONTEXT_LAYER_3_KERNEL,
    context_decoder_params,
};
pub use context_pipeline::{
    ContextModelParams, ContextPipelineWorkspace, decode_context_and_select_base_models,
    decode_context_and_select_base_models_default, decode_context_stddev_into,
    default_context_model_params, dequantize_context_latents_into,
    select_base_range_models_into,
};
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
pub use entropy::{
    decode_base_latents, decode_base_latents_into, decode_context_latents,
    decode_context_latents_into,
};
pub use fd_lsf::{
    FD_SHAPING_SFB_BOUNDARIES, FD_SHAPING_SUBBANDS, LSF_MEAN, LSF_ORDER,
    LsfCodebook, LsfCodebooks, dequantize_lsf,
};
pub use fd_lsf_tables::{
    FD_LSF_TABLE_BYTES, FD_LSF_TABLE_FNV1A, FD_LSF_TABLE_VALUES,
    normative_lsf_codebooks,
};
pub use fd_shaping::{
    FdShapingWorkspace, apply_inverse_fd_spectrum_shaping, lsf_to_lsp, lsp_to_lpc,
};
pub use frame::{AATF_SYNCWORD, AatfFrameHeader, SoundBedType, parse_aatf_frame_header};
pub use ga::{GaCodecFormat, GaDecodePlan, coded_payload};
pub use ga_frame::{
    GaChannelSideInfo, GaMultichannelFrameSideInfo, parse_multichannel_frame_side_info,
};
pub use ga_mono::{GaMonoFrameSideInfo, parse_mono_frame_side_info};
pub use ga_mono_neural::{
    BasicMonoNeuralWorkspace, decode_basic_channel_neural_mdct,
    decode_basic_mono_neural_mdct, parse_and_decode_basic_mono_neural_mdct,
};
pub use ga_mono_pcm::parse_decode_basic_mono_pcm;
pub use ga_mono_post::{
    BasicMonoPreFdWorkspace, BasicMonoSynthesisWorkspace, parse_decode_basic_mono_pre_fd,
};
pub use ga_stereo::{
    GaStereoFrameSideInfo, StereoCouplingSideInfo, StereoSideInfo,
    allocate_stereo_ms_bytes, parse_stereo_frame_side_info, parse_stereo_side_info_at,
};
pub use ga_stereo_pcm::{BasicStereoSynthesisWorkspace, parse_decode_basic_stereo_pcm};
pub use group::{
    GroupSideInfo, SpectrumDegroupWorkspace, inverse_group_spectrum, parse_group_bits_at,
};
pub use imdct_synthesis::{Avs3SynthesisWorkspace, synthesize_mdct_frame};
pub use inverse_qc::{
    basic_feature_scale, inverse_scale_in_place, low_complexity_feature_scale,
    noise_filling_parameter,
};
pub use isobmff::{Av3aIsoBmffDemuxer, Av3aSampleTiming};
pub use metadata::{MetadataBoundary, parse_metadata_boundary};
pub use metadata_prefix::{
    DynamicChannelPrefix, DynamicMetadataPrefix, StaticMetadataPrefix,
    parse_dynamic_channel_prefix_at, parse_dynamic_metadata_prefix_at,
    parse_static_metadata_prefix_at,
};
pub use multichannel::{
    MultichannelPairSideInfo, MultichannelSideInfo, channel_pair_index_bits,
    parse_multichannel_side_info_at,
};
pub use neural::{
    CONTEXT_CHANNELS, CONTEXT_INPUT_POSITIONS, CONTEXT_LAYER_1_SPEC, CONTEXT_LAYER_2_SPEC,
    CONTEXT_LAYER_3_SPEC, CONTEXT_OUTPUT_POSITIONS, ContextDecoderParams, ContextDecoderWorkspace,
    ConvTranspose1dParams, ConvTranspose1dSpec, NeuralActivation, conv1d_transpose_same,
    decode_context_network,
};
pub use qc::{BitRange, QcSideInfo, parse_qc_side_info_at, qc_fixed_header_bits};
pub use quantizer_params::{BASE_QUANTILE_MEDIANS, CONTEXT_QUANTILE_MEDIANS};
pub use range::{
    RANGE_DEFAULT_PRECISION, RANGE_OVERFLOW_WIDTH, RangeByteWindow, RangeDecoder, RangeModel,
};
pub use range_tables::{
    BASE_RANGE_MODEL_COUNT, BASE_RANGE_MODEL_OFFSETS, BASE_STDDEV_THRESHOLD_BITS,
    CONTEXT_RANGE_MODEL_COUNT, base_range_model, base_stddev_threshold, context_range_model,
    select_base_range_model_index,
};
pub use stereo_synthesis::apply_stereo_ms_upmix;
pub use synthesis::{apply_window_in_place, overlap_add, scale_pcm_in_place, spectral_dot};
pub use tns::{
    TNS_REFLECTION_COEFFICIENTS, TnsFilterSideInfo, TnsSideInfo,
    parse_tns_side_info_at, reflection_coefficient,
};
pub use tns_synthesis::{TnsSynthesisWorkspace, apply_inverse_tns};
