use yinqidao_codec_core::CodecError;

use crate::{
    BASE_OUTPUT_POSITIONS, Avs3SynthesisWorkspace, BasicMonoNeuralWorkspace, BweConfig,
    BweSideInfo, BweSynthesisWorkspace, CoreSidePrefix, FdShapingWorkspace,
    GaStereoFrameSideInfo, GaStereoMcrFrameSideInfo, LsfCodebooks, NeuralNetworkType,
    SpectrumDegroupWorkspace, StereoSideInfo, TnsSynthesisWorkspace, apply_bwe_synthesis,
    apply_inverse_fd_spectrum_shaping, apply_inverse_tns, apply_mcr_stereo_upmix,
    apply_stereo_ms_upmix, decode_channel_neural_mdct, inverse_group_spectrum,
    normative_lsf_codebooks, parse_stereo_frame_side_info, parse_stereo_mcr_frame_side_info,
    synthesize_mdct_frame,
};

const STEREO_CHANNELS: usize = 2;
const STEREO_PCM_SAMPLES: usize = BASE_OUTPUT_POSITIONS * STEREO_CHANNELS;

/// Decoder-owned reusable state for conventional and MCR stereo across both neural profiles.
#[derive(Debug)]
pub struct BasicStereoSynthesisWorkspace {
    neural: [BasicMonoNeuralWorkspace; STEREO_CHANNELS],
    degroup: [SpectrumDegroupWorkspace; STEREO_CHANNELS],
    bwe: [BweSynthesisWorkspace; STEREO_CHANNELS],
    tns: [TnsSynthesisWorkspace; STEREO_CHANNELS],
    fd: [FdShapingWorkspace; STEREO_CHANNELS],
    synthesis: [Avs3SynthesisWorkspace; STEREO_CHANNELS],
    spectra: [[f32; BASE_OUTPUT_POSITIONS]; STEREO_CHANNELS],
    planar_pcm: [[f32; BASE_OUTPUT_POSITIONS]; STEREO_CHANNELS],
}

impl BasicStereoSynthesisWorkspace {
    pub fn new() -> Self {
        Self {
            neural: std::array::from_fn(|_| BasicMonoNeuralWorkspace::new()),
            degroup: std::array::from_fn(|_| SpectrumDegroupWorkspace::new()),
            bwe: std::array::from_fn(|_| BweSynthesisWorkspace::new()),
            tns: std::array::from_fn(|_| TnsSynthesisWorkspace::new()),
            fd: std::array::from_fn(|_| FdShapingWorkspace::new()),
            synthesis: std::array::from_fn(|_| Avs3SynthesisWorkspace::new()),
            spectra: [[0.0; BASE_OUTPUT_POSITIONS]; STEREO_CHANNELS],
            planar_pcm: [[0.0; BASE_OUTPUT_POSITIONS]; STEREO_CHANNELS],
        }
    }

    pub fn reset_synthesis_history(&mut self) {
        for synthesis in &mut self.synthesis {
            synthesis.reset();
        }
    }

    pub fn spectra(&self) -> &[[f32; BASE_OUTPUT_POSITIONS]; STEREO_CHANNELS] {
        &self.spectra
    }
}

impl Default for BasicStereoSynthesisWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GaStereoPcmChannelInfo {
    pub core: CoreSidePrefix,
    pub bwe: Option<BweSideInfo>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GaStereoPcmSideInfo {
    pub channels: [GaStereoPcmChannelInfo; STEREO_CHANNELS],
    pub stereo: StereoSideInfo,
    pub is_mcr: bool,
    pub next_bit_offset: usize,
    pub trailing_bits: usize,
}

enum ParsedStereoSide {
    Conventional(GaStereoFrameSideInfo),
    Mcr(GaStereoMcrFrameSideInfo),
}

#[allow(clippy::too_many_arguments)]
fn post_synthesize_channel(
    core: CoreSidePrefix,
    bwe_side: Option<BweSideInfo>,
    bwe_config: Option<BweConfig>,
    codebooks: LsfCodebooks<'_>,
    spectrum: &mut [f32],
    bwe_workspace: &mut BweSynthesisWorkspace,
    tns_workspace: &mut TnsSynthesisWorkspace,
    fd_workspace: &mut FdShapingWorkspace,
    pcm: &mut [f32],
    synthesis_workspace: &mut Avs3SynthesisWorkspace,
) -> Result<(), CodecError> {
    match (bwe_config, bwe_side) {
        (Some(config), Some(side)) => {
            apply_bwe_synthesis(config, side, spectrum, bwe_workspace)?;
        }
        (None, None) => {}
        _ => {
            return Err(CodecError::InvalidData(
                "stereo BWE configuration and decoded side information disagree",
            ));
        }
    }

    apply_inverse_tns(&core.tns, core.transform_type, spectrum, tns_workspace)?;
    apply_inverse_fd_spectrum_shaping(
        &core.fd_shaping,
        codebooks,
        spectrum,
        fd_workspace,
    )?;
    synthesize_mdct_frame(
        core.transform_type,
        spectrum,
        pcm,
        synthesis_workspace,
    )
}

fn decode_conventional_stereo(
    nn_type: NeuralNetworkType,
    payload: &[u8],
    side: &GaStereoFrameSideInfo,
    workspace: &mut BasicStereoSynthesisWorkspace,
) -> Result<(), CodecError> {
    for channel_index in 0..STEREO_CHANNELS {
        let channel = &side.channels[channel_index];
        decode_channel_neural_mdct(
            nn_type,
            payload,
            channel,
            side.bwe_config,
            &mut workspace.neural[channel_index],
            &mut workspace.spectra[channel_index],
        )?;
        inverse_group_spectrum(
            channel.core.transform_type,
            channel.group,
            &mut workspace.spectra[channel_index],
            &mut workspace.degroup[channel_index],
        )?;
    }

    let (left, right) = workspace.spectra.split_at_mut(1);
    apply_stereo_ms_upmix(side.stereo, &mut left[0], &mut right[0])
}

fn decode_mcr_stereo(
    nn_type: NeuralNetworkType,
    payload: &[u8],
    side: &GaStereoMcrFrameSideInfo,
    workspace: &mut BasicStereoSynthesisWorkspace,
) -> Result<(), CodecError> {
    decode_channel_neural_mdct(
        nn_type,
        payload,
        &side.left,
        side.bwe_config,
        &mut workspace.neural[0],
        &mut workspace.spectra[0],
    )?;
    inverse_group_spectrum(
        side.left.core.transform_type,
        side.left.group,
        &mut workspace.spectra[0],
        &mut workspace.degroup[0],
    )?;

    let (left, right) = workspace.spectra.split_at_mut(1);
    apply_mcr_stereo_upmix(side.stereo, &mut left[0], &mut right[0])
}

/// Decode one Basic or Low-Complexity stereo payload to 1024 interleaved PCM frames.
///
/// >32 kb/s uses conventional two-channel inverse-QC + M/S/ILD. <=32 kb/s uses one coded neural
/// channel followed by the normative MCR reconstruction. Both modes then share channel-local
/// BWE/TNS/FD shaping and IMDCT/OLA.
pub fn parse_decode_stereo_pcm(
    nn_type: NeuralNetworkType,
    payload: &[u8],
    core_bit_offset: usize,
    low_bitrate_precision: bool,
    bwe_config: Option<BweConfig>,
    total_bitrate_kbps: u32,
    workspace: &mut BasicStereoSynthesisWorkspace,
    pcm_interleaved: &mut [f32],
) -> Result<GaStereoPcmSideInfo, CodecError> {
    if matches!(nn_type, NeuralNetworkType::Reserved(_)) {
        return Err(CodecError::Unsupported(
            "reserved AVS3 neural-network type in stereo synthesis",
        ));
    }
    if pcm_interleaved.len() != STEREO_PCM_SAMPLES {
        return Err(CodecError::InvalidData(
            "stereo synthesis output must contain 2048 interleaved PCM samples",
        ));
    }

    let parsed = if total_bitrate_kbps <= 32 {
        let side = parse_stereo_mcr_frame_side_info(
            payload,
            core_bit_offset,
            nn_type,
            low_bitrate_precision,
            bwe_config,
            total_bitrate_kbps,
        )?;
        decode_mcr_stereo(nn_type, payload, &side, workspace)?;
        ParsedStereoSide::Mcr(side)
    } else {
        let side = parse_stereo_frame_side_info(
            payload,
            core_bit_offset,
            nn_type,
            low_bitrate_precision,
            bwe_config,
            total_bitrate_kbps,
        )?;
        decode_conventional_stereo(nn_type, payload, &side, workspace)?;
        ParsedStereoSide::Conventional(side)
    };

    let codebooks = normative_lsf_codebooks();
    let diagnostics = match &parsed {
        ParsedStereoSide::Conventional(frame) => {
            for channel_index in 0..STEREO_CHANNELS {
                let channel = &frame.channels[channel_index];
                post_synthesize_channel(
                    channel.core,
                    channel.bwe,
                    frame.bwe_config,
                    codebooks,
                    &mut workspace.spectra[channel_index],
                    &mut workspace.bwe[channel_index],
                    &mut workspace.tns[channel_index],
                    &mut workspace.fd[channel_index],
                    &mut workspace.planar_pcm[channel_index],
                    &mut workspace.synthesis[channel_index],
                )?;
            }
            GaStereoPcmSideInfo {
                channels: frame.channels.each_ref().map(|channel| GaStereoPcmChannelInfo {
                    core: channel.core,
                    bwe: channel.bwe,
                }),
                stereo: frame.stereo,
                is_mcr: false,
                next_bit_offset: frame.next_bit_offset,
                trailing_bits: frame.trailing_bits,
            }
        }
        ParsedStereoSide::Mcr(frame) => {
            post_synthesize_channel(
                frame.left.core,
                frame.left.bwe,
                frame.bwe_config,
                codebooks,
                &mut workspace.spectra[0],
                &mut workspace.bwe[0],
                &mut workspace.tns[0],
                &mut workspace.fd[0],
                &mut workspace.planar_pcm[0],
                &mut workspace.synthesis[0],
            )?;
            post_synthesize_channel(
                frame.right_core,
                frame.right_bwe,
                frame.bwe_config,
                codebooks,
                &mut workspace.spectra[1],
                &mut workspace.bwe[1],
                &mut workspace.tns[1],
                &mut workspace.fd[1],
                &mut workspace.planar_pcm[1],
                &mut workspace.synthesis[1],
            )?;
            GaStereoPcmSideInfo {
                channels: [
                    GaStereoPcmChannelInfo {
                        core: frame.left.core,
                        bwe: frame.left.bwe,
                    },
                    GaStereoPcmChannelInfo {
                        core: frame.right_core,
                        bwe: frame.right_bwe,
                    },
                ],
                stereo: frame.stereo,
                is_mcr: true,
                next_bit_offset: frame.next_bit_offset,
                trailing_bits: frame.trailing_bits,
            }
        }
    };

    for frame in 0..BASE_OUTPUT_POSITIONS {
        pcm_interleaved[2 * frame] = workspace.planar_pcm[0][frame];
        pcm_interleaved[2 * frame + 1] = workspace.planar_pcm[1][frame];
    }
    Ok(diagnostics)
}

/// Compatibility Basic-profile stereo entry point.
pub fn parse_decode_basic_stereo_pcm(
    payload: &[u8],
    core_bit_offset: usize,
    low_bitrate_precision: bool,
    bwe_config: Option<BweConfig>,
    total_bitrate_kbps: u32,
    workspace: &mut BasicStereoSynthesisWorkspace,
    pcm_interleaved: &mut [f32],
) -> Result<GaStereoPcmSideInfo, CodecError> {
    parse_decode_stereo_pcm(
        NeuralNetworkType::Basic,
        payload,
        core_bit_offset,
        low_bitrate_precision,
        bwe_config,
        total_bitrate_kbps,
        workspace,
        pcm_interleaved,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_interleaved_output_geometry_before_parsing() {
        let mut workspace = BasicStereoSynthesisWorkspace::new();
        let mut pcm = [0.0_f32; BASE_OUTPUT_POSITIONS];
        assert!(parse_decode_stereo_pcm(
            NeuralNetworkType::LowComplexity,
            &[],
            0,
            false,
            None,
            64,
            &mut workspace,
            &mut pcm,
        )
        .is_err());
    }

    #[test]
    fn truncated_low_complexity_mcr_payload_selects_mcr_layout() {
        let mut workspace = BasicStereoSynthesisWorkspace::new();
        let mut pcm = [0.0_f32; STEREO_PCM_SAMPLES];
        assert!(parse_decode_stereo_pcm(
            NeuralNetworkType::LowComplexity,
            &[],
            0,
            false,
            None,
            32,
            &mut workspace,
            &mut pcm,
        )
        .is_err());
    }
}
