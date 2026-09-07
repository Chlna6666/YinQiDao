use yinqidao_codec_core::CodecError;

use crate::{
    BASE_OUTPUT_POSITIONS, BasePipelineWorkspace, BweConfig, ContextPipelineWorkspace,
    GaMonoFrameSideInfo, NeuralNetworkType, NoiseFillingRng, decode_basic_base_to_mdct_normative,
    decode_context_and_select_base_models_default, parse_mono_frame_side_info,
};

const BASE_MODEL_VALUES: usize = 64 * 16;

/// Reusable scratch/state for the basic-profile mono hyper-prior path.
///
/// The 1024-value context output and B.9 model-index map are kept directly in the decoder-owned
/// workspace. Context/base neural workspaces retain their internal buffers across frames and the
/// noise-filling PRNG is decoder-local, avoiding global RNG contention and per-frame allocation.
#[derive(Debug)]
pub struct BasicMonoNeuralWorkspace {
    context: ContextPipelineWorkspace,
    base: BasePipelineWorkspace,
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
                "mono BWE configuration is missing its start line",
            )),
        None => Ok(BASE_OUTPUT_POSITIONS),
    }
}

/// Decode one already-parsed basic-profile mono QC payload to the 1024-point neural MDCT spectrum.
///
/// Data flow:
/// `contextBitstream -> context range decode -> inverse quantization -> B.2..B.7 -> B.8 -> B.9
/// -> base range decode -> inverse quantization/noise filling/scale -> B.10..B.23 -> MDCT`.
///
/// `side` and both entropy ranges reference `payload` directly, so the function copies no coded
/// payload bytes. All mutable scratch is retained in `workspace` across frames.
pub fn decode_basic_mono_neural_mdct(
    payload: &[u8],
    side: &GaMonoFrameSideInfo,
    workspace: &mut BasicMonoNeuralWorkspace,
    output: &mut [f32],
) -> Result<(), CodecError> {
    if output.len() != BASE_OUTPUT_POSITIONS {
        return Err(CodecError::InvalidData(
            "basic mono neural output must contain 1024 MDCT coefficients",
        ));
    }

    let qc = &side.channel.qc;
    let is_feat_amplified = qc.is_feat_amplified.ok_or(CodecError::InvalidData(
        "basic mono QC is missing isFeatAmplified",
    ))?;
    let scale_q_idx = qc.scale_q_idx.ok_or(CodecError::InvalidData(
        "basic mono QC is missing scaleQIdx",
    ))?;

    decode_context_and_select_base_models_default(
        payload,
        qc.context_bitstream,
        &mut workspace.context,
        &mut workspace.context_stddev,
        &mut workspace.model_indices,
    )?;

    decode_basic_base_to_mdct_normative(
        payload,
        qc.base_bitstream,
        &workspace.model_indices,
        noise_fill_line_count(side.bwe_config)?,
        side.channel.group,
        qc.nf_param_q_idx,
        is_feat_amplified,
        scale_q_idx,
        &mut workspace.rng,
        &mut workspace.base,
        output,
    )
}

/// Parse the complete mono syntax through QC and immediately execute the built-in Basic neural
/// inverse-QC model. This is the thin production front-end intended for `Avs3Decoder`.
///
/// The returned side-info owns only parsed scalar metadata; context/base entropy payloads remain
/// zero-copy bit ranges into `payload`.
pub fn parse_and_decode_basic_mono_neural_mdct(
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
        NeuralNetworkType::Basic,
        low_bitrate_precision,
        bwe_config,
    )?;
    decode_basic_mono_neural_mdct(payload, &side, workspace, output)?;
    Ok(side)
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
    fn parse_and_decode_frontend_rejects_truncated_core_before_neural_work() {
        let mut workspace = BasicMonoNeuralWorkspace::new();
        let mut output = [0.0_f32; BASE_OUTPUT_POSITIONS];
        assert!(parse_and_decode_basic_mono_neural_mdct(
            &[],
            0,
            false,
            None,
            &mut workspace,
            &mut output,
        )
        .is_err());
    }
}
