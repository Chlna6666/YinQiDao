use yinqidao_codec_core::CodecError;

use crate::{
    BASE_OUTPUT_POSITIONS, Avs3SynthesisWorkspace, BasicMonoNeuralWorkspace, BweConfig,
    BweSynthesisWorkspace, FdShapingWorkspace, GaMonoFrameSideInfo, LsfCodebooks,
    NeuralNetworkType, SpectrumDegroupWorkspace, TnsSynthesisWorkspace, apply_bwe_synthesis,
    apply_inverse_fd_spectrum_shaping, apply_inverse_tns, inverse_group_spectrum,
    parse_and_decode_mono_neural_mdct, synthesize_mdct_frame,
};

/// Decoder-owned reusable state for the mono path through inverse TNS.
#[derive(Debug, Default)]
pub struct BasicMonoPreFdWorkspace {
    neural: BasicMonoNeuralWorkspace,
    degroup: SpectrumDegroupWorkspace,
    bwe: BweSynthesisWorkspace,
    tns: TnsSynthesisWorkspace,
}

impl BasicMonoPreFdWorkspace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn neural(&self) -> &BasicMonoNeuralWorkspace {
        &self.neural
    }
}

/// Persistent state for the complete mono spectral and PCM path.
///
/// The same post-neural state is reused by Basic and Low-Complexity inverse-QC profiles. Neural
/// scratch, BWE/TNS state, FD-shaping buffers, FFT plans/scratch, overlap history and the
/// intermediate 1024-line spectrum all live here and are reused across frames.
#[derive(Debug, Default)]
pub struct BasicMonoSynthesisWorkspace {
    pre_fd: BasicMonoPreFdWorkspace,
    fd: FdShapingWorkspace,
    synthesis: Avs3SynthesisWorkspace,
    spectrum: [f32; BASE_OUTPUT_POSITIONS],
}

impl BasicMonoSynthesisWorkspace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pre_fd(&self) -> &BasicMonoPreFdWorkspace {
        &self.pre_fd
    }

    pub fn spectrum(&self) -> &[f32; BASE_OUTPUT_POSITIONS] {
        &self.spectrum
    }

    pub fn reset_synthesis_history(&mut self) {
        self.synthesis.reset();
    }
}

/// Parse and decode a mono frame through the complete pre-FD-shaping spectrum path.
pub fn parse_decode_mono_pre_fd(
    nn_type: NeuralNetworkType,
    payload: &[u8],
    core_bit_offset: usize,
    low_bitrate_precision: bool,
    bwe_config: Option<BweConfig>,
    workspace: &mut BasicMonoPreFdWorkspace,
    output: &mut [f32],
) -> Result<GaMonoFrameSideInfo, CodecError> {
    if output.len() != BASE_OUTPUT_POSITIONS {
        return Err(CodecError::InvalidData(
            "mono pre-FD output must contain 1024 MDCT coefficients",
        ));
    }

    let side = parse_and_decode_mono_neural_mdct(
        nn_type,
        payload,
        core_bit_offset,
        low_bitrate_precision,
        bwe_config,
        &mut workspace.neural,
        output,
    )?;

    let transform = side.channel.core.transform_type;
    inverse_group_spectrum(
        transform,
        side.channel.group,
        output,
        &mut workspace.degroup,
    )?;

    match (side.bwe_config, side.channel.bwe) {
        (Some(config), Some(bwe_side)) => {
            apply_bwe_synthesis(config, bwe_side, output, &mut workspace.bwe)?;
        }
        (None, None) => {}
        _ => {
            return Err(CodecError::InvalidData(
                "mono BWE configuration and decoded side information disagree",
            ));
        }
    }

    apply_inverse_tns(
        &side.channel.core.tns,
        transform,
        output,
        &mut workspace.tns,
    )?;
    Ok(side)
}

/// Compatibility Basic-profile pre-FD entry point.
pub fn parse_decode_basic_mono_pre_fd(
    payload: &[u8],
    core_bit_offset: usize,
    low_bitrate_precision: bool,
    bwe_config: Option<BweConfig>,
    workspace: &mut BasicMonoPreFdWorkspace,
    output: &mut [f32],
) -> Result<GaMonoFrameSideInfo, CodecError> {
    parse_decode_mono_pre_fd(
        NeuralNetworkType::Basic,
        payload,
        core_bit_offset,
        low_bitrate_precision,
        bwe_config,
        workspace,
        output,
    )
}

/// Decode one mono payload all the way to 1024 floating-point PCM samples.
pub fn parse_decode_mono_pcm_with_codebooks(
    nn_type: NeuralNetworkType,
    payload: &[u8],
    core_bit_offset: usize,
    low_bitrate_precision: bool,
    bwe_config: Option<BweConfig>,
    codebooks: LsfCodebooks<'_>,
    workspace: &mut BasicMonoSynthesisWorkspace,
    pcm: &mut [f32],
) -> Result<GaMonoFrameSideInfo, CodecError> {
    if pcm.len() != BASE_OUTPUT_POSITIONS {
        return Err(CodecError::InvalidData(
            "mono synthesis output must contain 1024 PCM samples",
        ));
    }

    let side = parse_decode_mono_pre_fd(
        nn_type,
        payload,
        core_bit_offset,
        low_bitrate_precision,
        bwe_config,
        &mut workspace.pre_fd,
        &mut workspace.spectrum,
    )?;
    let transform = side.channel.core.transform_type;
    apply_inverse_fd_spectrum_shaping(
        &side.channel.core.fd_shaping,
        codebooks,
        &mut workspace.spectrum,
        &mut workspace.fd,
    )?;
    synthesize_mdct_frame(
        transform,
        &workspace.spectrum,
        pcm,
        &mut workspace.synthesis,
    )?;
    Ok(side)
}

/// Compatibility Basic-profile mono PCM entry point with explicit LSF codebooks.
pub fn parse_decode_basic_mono_pcm(
    payload: &[u8],
    core_bit_offset: usize,
    low_bitrate_precision: bool,
    bwe_config: Option<BweConfig>,
    codebooks: LsfCodebooks<'_>,
    workspace: &mut BasicMonoSynthesisWorkspace,
    pcm: &mut [f32],
) -> Result<GaMonoFrameSideInfo, CodecError> {
    parse_decode_mono_pcm_with_codebooks(
        NeuralNetworkType::Basic,
        payload,
        core_bit_offset,
        low_bitrate_precision,
        bwe_config,
        codebooks,
        workspace,
        pcm,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LsfCodebook;

    fn dummy_codebooks() -> LsfCodebooks<'static> {
        static DUMMY: [f32; 1] = [0.0];
        LsfCodebooks {
            high_stage1: [LsfCodebook { values: &DUMMY }; 2],
            high_stage2: [LsfCodebook { values: &DUMMY }; 5],
            low_stage1: [LsfCodebook { values: &DUMMY }; 2],
            low_stage2: [LsfCodebook { values: &DUMMY }; 3],
        }
    }

    #[test]
    fn rejects_wrong_output_geometry_before_parsing() {
        let mut workspace = BasicMonoPreFdWorkspace::new();
        let mut output = [0.0_f32; 8];
        assert!(parse_decode_mono_pre_fd(
            NeuralNetworkType::LowComplexity,
            &[],
            0,
            false,
            None,
            &mut workspace,
            &mut output,
        )
        .is_err());
    }

    #[test]
    fn complete_pcm_frontend_checks_geometry_before_codebooks() {
        let mut workspace = BasicMonoSynthesisWorkspace::new();
        let mut pcm = [0.0_f32; 8];
        assert!(parse_decode_basic_mono_pcm(
            &[],
            0,
            false,
            None,
            dummy_codebooks(),
            &mut workspace,
            &mut pcm,
        )
        .is_err());
    }

    #[test]
    fn low_complexity_pcm_frontend_rejects_truncated_payload_before_table_lookup() {
        let mut workspace = BasicMonoSynthesisWorkspace::new();
        let mut pcm = [0.0_f32; BASE_OUTPUT_POSITIONS];
        assert!(parse_decode_mono_pcm_with_codebooks(
            NeuralNetworkType::LowComplexity,
            &[],
            0,
            false,
            None,
            dummy_codebooks(),
            &mut workspace,
            &mut pcm,
        )
        .is_err());
    }
}
