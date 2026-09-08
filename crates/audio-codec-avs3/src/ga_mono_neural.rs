use yinqidao_codec_core::CodecError;

use crate::{
    BASE_OUTPUT_POSITIONS, BasePipelineWorkspace, BweConfig, ContextPipelineWorkspace,
    GaChannelSideInfo, GaMonoFrameSideInfo, LowComplexityPipelineWorkspace, NeuralNetworkType,
    NoiseFillingRng, decode_basic_base_to_mdct_normative,
    decode_context_and_select_base_models_default, decode_low_complexity_base_to_mdct_normative,
    parse_mono_frame_side_info,
};

const BASE_MODEL_VALUES: usize = 64 * 16;

/// Reusable scratch/state for one AVS3 neural-coded channel's hyper-prior path.
#[derive(Debug)]
pub struct BasicMonoNeuralWorkspace {
    context: ContextPipelineWorkspace,
    base: BasePipelineWorkspace,
    low_complexity: LowComplexityPipelineWorkspace,
    rng: NoiseFillingRng,
    context_stddev: [f32; BASE_MODEL_VALUES],
    model_indices: [u8; BASE_MODEL_VALUES],
}

impl BasicMonoNeuralWorkspace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn context_stddev(&self) -> &[f32] {
        &self.context_stddev
    }

    pub fn model_indices(&self) -> &[u8] {
        &self.model_indices
    }
}

impl Default for BasicMonoNeuralWorkspace {
    fn default() -> Self {
        Self {
            context: ContextPipelineWorkspace::new(),
            base: BasePipelineWorkspace::new(),
            low_complexity: LowComplexityPipelineWorkspace::new(),
            rng: NoiseFillingRng::default(),
            context_stddev: [0.0; BASE_MODEL_VALUES],
            model_indices: [0; BASE_MODEL_VALUES],
        }
    }
}

#[inline]
fn noise_fill_line_count(bwe_config: Option<BweConfig>) -> Result<usize, CodecError> {
    match bwe_config {
        Some(config) => config.target_tiles[0]
            .map(usize::from)
            .ok_or(CodecError::InvalidData(
                "BWE configuration is missing its start line",
            )),
        None => Ok(BASE_OUTPUT_POSITIONS),
    }
}

fn decode_channel_neural_mdct_with_noise_lines_impl(
    nn_type: NeuralNetworkType,
    payload: &[u8],
    channel: &GaChannelSideInfo,
    num_lines_noise_fill: usize,
    workspace: &mut BasicMonoNeuralWorkspace,
    output: &mut [f32],
) -> Result<(), CodecError> {
    if output.len() != BASE_OUTPUT_POSITIONS {
        return Err(CodecError::InvalidData(
            "AVS3 channel neural output must contain 1024 MDCT coefficients",
        ));
    }
    if num_lines_noise_fill > BASE_OUTPUT_POSITIONS {
        return Err(CodecError::InvalidData(
            "AVS3 neural noise-fill line count exceeds MDCT geometry",
        ));
    }

    let qc = &channel.qc;
    decode_context_and_select_base_models_default(
        payload,
        qc.context_bitstream,
        &mut workspace.context,
        &mut workspace.context_stddev,
        &mut workspace.model_indices,
    )?;

    match nn_type {
        NeuralNetworkType::Basic => {
            let is_feat_amplified = qc.is_feat_amplified.ok_or(CodecError::InvalidData(
                "basic channel QC is missing isFeatAmplified",
            ))?;
            let scale_q_idx = qc.scale_q_idx.ok_or(CodecError::InvalidData(
                "basic channel QC is missing scaleQIdx",
            ))?;
            if qc.scale_q_idx_lc.is_some() {
                return Err(CodecError::InvalidData(
                    "basic channel QC unexpectedly contains low-complexity scale data",
                ));
            }

            decode_basic_base_to_mdct_normative(
                payload,
                qc.base_bitstream,
                &workspace.model_indices,
                num_lines_noise_fill,
                channel.group,
                qc.nf_param_q_idx,
                is_feat_amplified,
                scale_q_idx,
                &mut workspace.rng,
                &mut workspace.base,
                output,
            )
        }
        NeuralNetworkType::LowComplexity => {
            let scale_q_idx_lc = qc.scale_q_idx_lc.ok_or(CodecError::InvalidData(
                "low-complexity channel QC is missing scaleQIdxLc",
            ))?;
            if qc.is_feat_amplified.is_some() || qc.scale_q_idx.is_some() {
                return Err(CodecError::InvalidData(
                    "low-complexity channel QC unexpectedly contains Basic scale data",
                ));
            }

            decode_low_complexity_base_to_mdct_normative(
                payload,
                qc.base_bitstream,
                &workspace.model_indices,
                num_lines_noise_fill,
                channel.group,
                qc.nf_param_q_idx,
                scale_q_idx_lc,
                &mut workspace.rng,
                &mut workspace.low_complexity,
                output,
            )
        }
        NeuralNetworkType::Reserved(_) => Err(CodecError::Unsupported(
            "reserved AVS3 neural-network type in inverse-QC",
        )),
    }
}

/// Decode a parsed channel while explicitly supplying its normative noise-fill line boundary.
///
/// Channel-based profiles derive this from BWE (or 1024 when BWE is disabled). HOA additionally
/// has bitrate rows whose core line boundary is below 1024 even with BWE disabled.
pub fn decode_channel_neural_mdct_with_noise_fill_lines(
    nn_type: NeuralNetworkType,
    payload: &[u8],
    channel: &GaChannelSideInfo,
    num_lines_noise_fill: usize,
    workspace: &mut BasicMonoNeuralWorkspace,
    output: &mut [f32],
) -> Result<(), CodecError> {
    decode_channel_neural_mdct_with_noise_lines_impl(
        nn_type,
        payload,
        channel,
        num_lines_noise_fill,
        workspace,
        output,
    )
}

/// Decode one already-parsed Basic or Low-Complexity coded channel to a 1024-line MDCT spectrum.
pub fn decode_channel_neural_mdct(
    nn_type: NeuralNetworkType,
    payload: &[u8],
    channel: &GaChannelSideInfo,
    bwe_config: Option<BweConfig>,
    workspace: &mut BasicMonoNeuralWorkspace,
    output: &mut [f32],
) -> Result<(), CodecError> {
    decode_channel_neural_mdct_with_noise_lines_impl(
        nn_type,
        payload,
        channel,
        noise_fill_line_count(bwe_config)?,
        workspace,
        output,
    )
}

pub fn decode_basic_channel_neural_mdct(
    payload: &[u8],
    channel: &GaChannelSideInfo,
    bwe_config: Option<BweConfig>,
    workspace: &mut BasicMonoNeuralWorkspace,
    output: &mut [f32],
) -> Result<(), CodecError> {
    decode_channel_neural_mdct(
        NeuralNetworkType::Basic,
        payload,
        channel,
        bwe_config,
        workspace,
        output,
    )
}

pub fn decode_basic_mono_neural_mdct(
    payload: &[u8],
    side: &GaMonoFrameSideInfo,
    workspace: &mut BasicMonoNeuralWorkspace,
    output: &mut [f32],
) -> Result<(), CodecError> {
    decode_channel_neural_mdct(
        NeuralNetworkType::Basic,
        payload,
        &side.channel,
        side.bwe_config,
        workspace,
        output,
    )
}

pub fn decode_low_complexity_mono_neural_mdct(
    payload: &[u8],
    side: &GaMonoFrameSideInfo,
    workspace: &mut BasicMonoNeuralWorkspace,
    output: &mut [f32],
) -> Result<(), CodecError> {
    decode_channel_neural_mdct(
        NeuralNetworkType::LowComplexity,
        payload,
        &side.channel,
        side.bwe_config,
        workspace,
        output,
    )
}

pub fn parse_and_decode_mono_neural_mdct(
    nn_type: NeuralNetworkType,
    payload: &[u8],
    core_bit_offset: usize,
    low_bitrate_precision: bool,
    bwe_config: Option<BweConfig>,
    workspace: &mut BasicMonoNeuralWorkspace,
    output: &mut [f32],
) -> Result<GaMonoFrameSideInfo, CodecError> {
    let side = parse_mono_frame_side_info(
        payload,
        core_bit_offset,
        nn_type,
        low_bitrate_precision,
        bwe_config,
    )?;
    decode_channel_neural_mdct(
        nn_type,
        payload,
        &side.channel,
        side.bwe_config,
        workspace,
        output,
    )?;
    Ok(side)
}

pub fn parse_and_decode_basic_mono_neural_mdct(
    payload: &[u8],
    core_bit_offset: usize,
    low_bitrate_precision: bool,
    bwe_config: Option<BweConfig>,
    workspace: &mut BasicMonoNeuralWorkspace,
    output: &mut [f32],
) -> Result<GaMonoFrameSideInfo, CodecError> {
    parse_and_decode_mono_neural_mdct(
        NeuralNetworkType::Basic,
        payload,
        core_bit_offset,
        low_bitrate_precision,
        bwe_config,
        workspace,
        output,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BweMode;

    #[test]
    fn noise_fill_uses_full_frame_without_bwe() {
        assert_eq!(noise_fill_line_count(None).unwrap(), BASE_OUTPUT_POSITIONS);
    }

    #[test]
    fn noise_fill_uses_first_target_tile_with_bwe() {
        let config = BweConfig::for_bitrate(BweMode::Mono, 32)
            .unwrap()
            .expect("32 kb/s mono enables BWE");
        let expected = usize::from(config.target_tiles[0].unwrap());
        assert_eq!(noise_fill_line_count(Some(config)).unwrap(), expected);
    }

    #[test]
    fn basic_frontend_rejects_truncated_core_before_neural_work() {
        let mut workspace = BasicMonoNeuralWorkspace::new();
        let mut output = [0.0_f32; BASE_OUTPUT_POSITIONS];
        assert!(
            parse_and_decode_basic_mono_neural_mdct(
                &[],
                0,
                false,
                None,
                &mut workspace,
                &mut output,
            )
            .is_err()
        );
    }

    #[test]
    fn low_complexity_frontend_rejects_truncated_core_before_neural_work() {
        let mut workspace = BasicMonoNeuralWorkspace::new();
        let mut output = [0.0_f32; BASE_OUTPUT_POSITIONS];
        assert!(
            parse_and_decode_mono_neural_mdct(
                NeuralNetworkType::LowComplexity,
                &[],
                0,
                false,
                None,
                &mut workspace,
                &mut output,
            )
            .is_err()
        );
    }
}
