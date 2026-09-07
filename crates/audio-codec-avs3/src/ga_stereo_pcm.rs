use yinqidao_codec_core::CodecError;

use crate::{
    BASE_OUTPUT_POSITIONS, Avs3SynthesisWorkspace, BasicMonoNeuralWorkspace, BweConfig,
    BweSynthesisWorkspace, FdShapingWorkspace, GaStereoFrameSideInfo, NeuralNetworkType,
    SpectrumDegroupWorkspace, TnsSynthesisWorkspace, apply_bwe_synthesis,
    apply_inverse_fd_spectrum_shaping, apply_inverse_tns, apply_stereo_ms_upmix,
    decode_basic_channel_neural_mdct, inverse_group_spectrum, normative_lsf_codebooks,
    parse_stereo_frame_side_info, synthesize_mdct_frame,
};

const STEREO_CHANNELS: usize = 2;
const STEREO_PCM_SAMPLES: usize = BASE_OUTPUT_POSITIONS * STEREO_CHANNELS;

/// Decoder-owned reusable state for the complete Basic-profile conventional-stereo path.
///
/// Each coded/downmixed channel owns independent neural, degrouping, BWE/TNS, FD-shaping and
/// IMDCT/OLA state. The two 1024-line spectra are upmixed in place before post processing, matching
/// the normative Table-9 ordering. Planar PCM scratch is interleaved only at the final API boundary.
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

/// Decode one >32-kb/s conventional Basic-profile stereo payload to 1024 interleaved PCM frames.
///
/// Normative order:
/// `two-channel neural inverse-QC -> inverse grouping -> inverse M/S + ILD -> per-channel BWE ->
/// inverse TNS -> inverse FD shaping -> independent IMDCT/window/OLA -> interleave`.
///
/// MCR stereo remains an explicit unsupported path in `parse_stereo_frame_side_info()` until the
/// referenced GB/T 33475.3 B.154/B.155 reconstruction codebooks are installed.
pub fn parse_decode_basic_stereo_pcm(
    payload: &[u8],
    core_bit_offset: usize,
    low_bitrate_precision: bool,
    bwe_config: Option<BweConfig>,
    total_bitrate_kbps: u32,
    workspace: &mut BasicStereoSynthesisWorkspace,
    pcm_interleaved: &mut [f32],
) -> Result<GaStereoFrameSideInfo, CodecError> {
    if pcm_interleaved.len() != STEREO_PCM_SAMPLES {
        return Err(CodecError::InvalidData(
            "basic stereo synthesis output must contain 2048 interleaved PCM samples",
        ));
    }

    let side = parse_stereo_frame_side_info(
        payload,
        core_bit_offset,
        NeuralNetworkType::Basic,
        low_bitrate_precision,
        bwe_config,
        total_bitrate_kbps,
    )?;

    for channel_index in 0..STEREO_CHANNELS {
        let channel = &side.channels[channel_index];
        decode_basic_channel_neural_mdct(
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

    {
        let (left, right) = workspace.spectra.split_at_mut(1);
        apply_stereo_ms_upmix(side.stereo, &mut left[0], &mut right[0])?;
    }

    let codebooks = normative_lsf_codebooks();
    for channel_index in 0..STEREO_CHANNELS {
        let channel = &side.channels[channel_index];
        let spectrum = &mut workspace.spectra[channel_index];

        match (side.bwe_config, channel.bwe) {
            (Some(config), Some(bwe_side)) => {
                apply_bwe_synthesis(
                    config,
                    bwe_side,
                    spectrum,
                    &mut workspace.bwe[channel_index],
                )?;
            }
            (None, None) => {}
            _ => {
                return Err(CodecError::InvalidData(
                    "stereo BWE configuration and decoded side information disagree",
                ));
            }
        }

        apply_inverse_tns(
            &channel.core.tns,
            channel.core.transform_type,
            spectrum,
            &mut workspace.tns[channel_index],
        )?;
        apply_inverse_fd_spectrum_shaping(
            &channel.core.fd_shaping,
            codebooks,
            spectrum,
            &mut workspace.fd[channel_index],
        )?;
        synthesize_mdct_frame(
            channel.core.transform_type,
            spectrum,
            &mut workspace.planar_pcm[channel_index],
            &mut workspace.synthesis[channel_index],
        )?;
    }

    for frame in 0..BASE_OUTPUT_POSITIONS {
        pcm_interleaved[2 * frame] = workspace.planar_pcm[0][frame];
        pcm_interleaved[2 * frame + 1] = workspace.planar_pcm[1][frame];
    }
    Ok(side)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_interleaved_output_geometry_before_parsing() {
        let mut workspace = BasicStereoSynthesisWorkspace::new();
        let mut pcm = [0.0_f32; BASE_OUTPUT_POSITIONS];
        assert!(parse_decode_basic_stereo_pcm(
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
    fn truncated_payload_fails_before_neural_or_post_processing() {
        let mut workspace = BasicStereoSynthesisWorkspace::new();
        let mut pcm = [0.0_f32; STEREO_PCM_SAMPLES];
        assert!(parse_decode_basic_stereo_pcm(
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
}
