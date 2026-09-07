use yinqidao_codec_core::CodecError;

use crate::{
    BASE_OUTPUT_POSITIONS, Avs3SynthesisWorkspace, BasicMonoNeuralWorkspace, BweConfig,
    BweSynthesisWorkspace, FdShapingWorkspace, GaMonoFrameSideInfo, LsfCodebooks,
    SpectrumDegroupWorkspace, TnsSynthesisWorkspace, apply_bwe_synthesis,
    apply_inverse_fd_spectrum_shaping, apply_inverse_tns, inverse_group_spectrum,
    parse_and_decode_basic_mono_neural_mdct, synthesize_mdct_frame,
};

/// Decoder-owned reusable state for the Basic mono path through inverse TNS.
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

/// Persistent state for the complete Basic-profile mono spectral and PCM path.
///
/// Neural scratch, BWE/TNS state, FD-shaping buffers, FFT plans/scratch, overlap history and the
/// intermediate 1024-line spectrum all live here and are reused across frames. Once constructed,
/// decoding one frame does not allocate in the post-neural path.
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

    /// Most recently reconstructed spectrum after inverse FD shaping.
    pub fn spectrum(&self) -> &[f32; BASE_OUTPUT_POSITIONS] {
        &self.spectrum
    }

    pub fn reset_synthesis_history(&mut self) {
        self.synthesis.reset();
    }
}

/// Parse and decode a Basic-profile mono frame through the complete pre-FD-shaping spectrum path.
///
/// Order:
/// `QC/neural inverse -> spectrum inverse grouping -> BWE -> inverse TNS`.
///
/// The returned 1024-line spectrum is intentionally left before frequency-domain inverse spectrum
/// shaping so callers that need a spectral-domain stage can reuse this boundary directly.
pub fn parse_decode_basic_mono_pre_fd(
    payload: &[u8],
    core_bit_offset: usize,
    low_bitrate_precision: bool,
    bwe_config: Option<BweConfig>,
    workspace: &mut BasicMonoPreFdWorkspace,
    output: &mut [f32],
) -> Result<GaMonoFrameSideInfo, CodecError> {
    if output.len() != BASE_OUTPUT_POSITIONS {
        return Err(CodecError::InvalidData(
            "basic mono pre-FD output must contain 1024 MDCT coefficients",
        ));
    }

    let side = parse_and_decode_basic_mono_neural_mdct(
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

/// Decode one Basic-profile mono payload all the way to 1024 floating-point PCM samples.
///
/// Normative order:
/// `neural inverse-QC -> inverse grouping -> BWE -> inverse TNS -> inverse FD shaping -> IMDCT ->
/// window -> overlap/add`.
///
/// `codebooks` is currently explicit because Annex-B B.34..B.45 are the final large normative
/// asset not yet bundled in this crate. The execution path itself is complete; once the static
/// asset is installed this parameter can be replaced by `normative_lsf_codebooks()` without
/// changing the hot path.
pub fn parse_decode_basic_mono_pcm(
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
            "basic mono synthesis output must contain 1024 PCM samples",
        ));
    }

    let side = parse_decode_basic_mono_pre_fd(
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
        assert!(parse_decode_basic_mono_pre_fd(
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
    fn truncated_payload_fails_before_post_processing() {
        let mut workspace = BasicMonoPreFdWorkspace::new();
        let mut output = [0.0_f32; BASE_OUTPUT_POSITIONS];
        assert!(parse_decode_basic_mono_pre_fd(
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
    fn complete_pcm_frontend_rejects_truncated_payload_before_table_lookup() {
        let mut workspace = BasicMonoSynthesisWorkspace::new();
        let mut pcm = [0.0_f32; BASE_OUTPUT_POSITIONS];
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
}
