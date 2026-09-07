use yinqidao_codec_core::CodecError;

use crate::{
    BASE_OUTPUT_POSITIONS, Avs3SynthesisWorkspace, BasicMonoNeuralWorkspace, BweConfig,
    BweSynthesisWorkspace, FdShapingWorkspace, GaMultichannelFrameSideInfo, NeuralNetworkType,
    SpectrumDegroupWorkspace, TnsSynthesisWorkspace, apply_bwe_synthesis,
    apply_inverse_fd_spectrum_shaping, apply_inverse_tns, apply_multichannel_mcac,
    decode_basic_channel_neural_mdct, inverse_group_spectrum, normative_lsf_codebooks,
    parse_multichannel_frame_side_info, synthesize_mdct_frame,
};

/// Number of MDCT coefficients retained for an AVS3 multichannel LFE signal.
///
/// The published UWA/Huawei reference decoder defines `LFE_RESERVED_LINES` as 32 and clears all
/// higher coefficients in `McLfeProc()` after inverse FD spectrum shaping and before IMDCT. At
/// 48 kHz with the normative 1024-line long transform this corresponds to the intended ~750-Hz LFE
/// bandwidth. Short-window spectra remain in the codec's eight-way frequency-interleaved layout at
/// this point; keeping the first 32 entries is therefore equivalent to the reference decoder's
/// pre-deinterleave restriction.
pub const MC_LFE_RESERVED_LINES: usize = 32;

/// Decoder-owned reusable state for the complete Basic-profile multichannel path.
///
/// Workspaces are provisioned once for the configured channel count and then retained across
/// frames. Each coded channel owns independent neural, inverse-grouping, BWE/TNS, FD-shaping and
/// IMDCT/OLA state. Spectra are coupled in place by MCAC before the per-channel post-synthesis pass.
#[derive(Debug, Default)]
pub struct BasicMultichannelSynthesisWorkspace {
    neural: Vec<BasicMonoNeuralWorkspace>,
    degroup: Vec<SpectrumDegroupWorkspace>,
    bwe: Vec<BweSynthesisWorkspace>,
    tns: Vec<TnsSynthesisWorkspace>,
    fd: Vec<FdShapingWorkspace>,
    synthesis: Vec<Avs3SynthesisWorkspace>,
    spectra: Vec<[f32; BASE_OUTPUT_POSITIONS]>,
    planar_pcm: Vec<[f32; BASE_OUTPUT_POSITIONS]>,
}

impl BasicMultichannelSynthesisWorkspace {
    pub fn new() -> Self {
        Self::default()
    }

    fn prepare_channels(&mut self, channel_count: usize) {
        if self.neural.len() == channel_count {
            return;
        }

        self.neural.clear();
        self.degroup.clear();
        self.bwe.clear();
        self.tns.clear();
        self.fd.clear();
        self.synthesis.clear();
        self.spectra.clear();
        self.planar_pcm.clear();

        self.neural.reserve(channel_count);
        self.degroup.reserve(channel_count);
        self.bwe.reserve(channel_count);
        self.tns.reserve(channel_count);
        self.fd.reserve(channel_count);
        self.synthesis.reserve(channel_count);
        self.spectra.reserve(channel_count);
        self.planar_pcm.reserve(channel_count);

        for _ in 0..channel_count {
            self.neural.push(BasicMonoNeuralWorkspace::new());
            self.degroup.push(SpectrumDegroupWorkspace::new());
            self.bwe.push(BweSynthesisWorkspace::new());
            self.tns.push(TnsSynthesisWorkspace::new());
            self.fd.push(FdShapingWorkspace::new());
            self.synthesis.push(Avs3SynthesisWorkspace::new());
            self.spectra.push([0.0; BASE_OUTPUT_POSITIONS]);
            self.planar_pcm.push([0.0; BASE_OUTPUT_POSITIONS]);
        }
    }

    pub fn reset_synthesis_history(&mut self) {
        for synthesis in &mut self.synthesis {
            synthesis.reset();
        }
    }

    pub fn spectra(&self) -> &[[f32; BASE_OUTPUT_POSITIONS]] {
        &self.spectra
    }

    pub fn channel_count(&self) -> usize {
        self.neural.len()
    }
}

/// Apply the normative multichannel LFE high-frequency restriction in-place.
///
/// This must run after BWE, inverse TNS and inverse FD spectrum shaping, but before IMDCT/OLA. It is
/// intentionally separate from MCAC because the LFE channel does not participate in MCAC coupling.
pub fn apply_multichannel_lfe_restriction(spectrum: &mut [f32]) -> Result<(), CodecError> {
    if spectrum.len() != BASE_OUTPUT_POSITIONS {
        return Err(CodecError::InvalidData(
            "multichannel LFE restriction requires a 1024-line MDCT spectrum",
        ));
    }
    spectrum[MC_LFE_RESERVED_LINES..].fill(0.0);
    Ok(())
}

/// Decode one Basic-profile multichannel payload to 1024 interleaved N-channel PCM frames.
///
/// Normative execution order:
/// `all channel inverse-QC -> all inverse grouping -> MCAC -> per-channel BWE -> inverse TNS ->
/// inverse FD shaping -> LFE 32-line restriction -> per-channel IMDCT/window/OLA -> interleave`.
///
/// The LFE restriction is applied only to `lfe_index`; pair indices consumed by MCAC address the
/// non-LFE logical channel list and are mapped back to coded-channel positions by
/// [`apply_multichannel_mcac`].
#[allow(clippy::too_many_arguments)]
pub fn parse_decode_basic_multichannel_pcm(
    payload: &[u8],
    core_bit_offset: usize,
    low_bitrate_precision: bool,
    bwe_config: Option<BweConfig>,
    total_bitrate_kbps: u32,
    channel_count: u16,
    lfe_index: Option<usize>,
    workspace: &mut BasicMultichannelSynthesisWorkspace,
    pcm_interleaved: &mut [f32],
) -> Result<GaMultichannelFrameSideInfo, CodecError> {
    if channel_count < 3 {
        return Err(CodecError::InvalidData(
            "basic multichannel synthesis requires at least three channels",
        ));
    }
    let channel_count_usize = usize::from(channel_count);
    if let Some(index) = lfe_index
        && index >= channel_count_usize
    {
        return Err(CodecError::InvalidData(
            "multichannel synthesis LFE index exceeds channel count",
        ));
    }
    let expected_samples = channel_count_usize
        .checked_mul(BASE_OUTPUT_POSITIONS)
        .ok_or(CodecError::InvalidData(
            "multichannel PCM output geometry overflows address space",
        ))?;
    if pcm_interleaved.len() != expected_samples {
        return Err(CodecError::InvalidData(
            "basic multichannel synthesis output has invalid interleaved PCM geometry",
        ));
    }

    let side = parse_multichannel_frame_side_info(
        payload,
        core_bit_offset,
        channel_count,
        NeuralNetworkType::Basic,
        low_bitrate_precision,
        bwe_config,
        total_bitrate_kbps,
        lfe_index,
    )?;
    if side.channels.len() != channel_count_usize || side.allocation.lfe_index != lfe_index {
        return Err(CodecError::Internal(
            "multichannel parser returned inconsistent channel geometry".into(),
        ));
    }

    workspace.prepare_channels(channel_count_usize);

    for channel_index in 0..channel_count_usize {
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

    apply_multichannel_mcac(&side.multichannel, lfe_index, &mut workspace.spectra)?;

    let codebooks = normative_lsf_codebooks();
    for channel_index in 0..channel_count_usize {
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
                    "multichannel BWE configuration and decoded side information disagree",
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
        if Some(channel_index) == lfe_index {
            apply_multichannel_lfe_restriction(spectrum)?;
        }
        synthesize_mdct_frame(
            channel.core.transform_type,
            spectrum,
            &mut workspace.planar_pcm[channel_index],
            &mut workspace.synthesis[channel_index],
        )?;
    }

    for frame in 0..BASE_OUTPUT_POSITIONS {
        let output_base = frame * channel_count_usize;
        for channel in 0..channel_count_usize {
            pcm_interleaved[output_base + channel] = workspace.planar_pcm[channel][frame];
        }
    }

    Ok(side)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lfe_restriction_preserves_exactly_first_32_mdct_entries() {
        let mut spectrum: [f32; BASE_OUTPUT_POSITIONS] =
            std::array::from_fn(|index| index as f32 + 1.0);
        let preserved = spectrum[..MC_LFE_RESERVED_LINES].to_vec();

        apply_multichannel_lfe_restriction(&mut spectrum).unwrap();

        assert_eq!(&spectrum[..MC_LFE_RESERVED_LINES], preserved.as_slice());
        assert!(spectrum[MC_LFE_RESERVED_LINES..].iter().all(|&value| value == 0.0));
    }

    #[test]
    fn lfe_restriction_rejects_non_normative_spectrum_geometry() {
        let mut spectrum = [1.0_f32; 32];
        assert!(apply_multichannel_lfe_restriction(&mut spectrum).is_err());
    }

    #[test]
    fn multichannel_frontend_rejects_invalid_output_geometry_before_parsing() {
        let mut workspace = BasicMultichannelSynthesisWorkspace::new();
        let mut pcm = [0.0_f32; BASE_OUTPUT_POSITIONS];
        assert!(parse_decode_basic_multichannel_pcm(
            &[],
            0,
            false,
            None,
            384,
            6,
            Some(3),
            &mut workspace,
            &mut pcm,
        )
        .is_err());
        assert_eq!(workspace.channel_count(), 0);
    }

    #[test]
    fn workspace_rebuilds_channel_state_as_one_coherent_geometry() {
        let mut workspace = BasicMultichannelSynthesisWorkspace::new();
        workspace.prepare_channels(6);
        assert_eq!(workspace.channel_count(), 6);
        assert_eq!(workspace.spectra().len(), 6);
        workspace.prepare_channels(10);
        assert_eq!(workspace.channel_count(), 10);
        assert_eq!(workspace.spectra().len(), 10);
    }
}
