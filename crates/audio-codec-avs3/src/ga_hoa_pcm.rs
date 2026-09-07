use yinqidao_codec_core::CodecError;

use crate::{
    BASE_OUTPUT_POSITIONS, Avs3SynthesisWorkspace, BasicMonoNeuralWorkspace,
    BweSynthesisWorkspace, FdShapingWorkspace, GaHoaFrameSideInfo, HoaConfig, HoaSideInfo,
    NeuralNetworkType, SpectrumDegroupWorkspace, TnsSynthesisWorkspace, apply_bwe_synthesis,
    apply_inverse_fd_spectrum_shaping, apply_inverse_tns,
    decode_channel_neural_mdct_with_noise_fill_lines, inverse_group_spectrum, mc_ild_factor,
    normative_lsf_codebooks, parse_hoa_frame_side_info, synthesize_mdct_frame,
};

/// Decoder-owned state for the HOA transport-channel stage before spatial basis recovery.
#[derive(Debug, Default)]
pub struct HoaTransportSynthesisWorkspace {
    neural: Vec<BasicMonoNeuralWorkspace>,
    degroup: Vec<SpectrumDegroupWorkspace>,
    bwe: Vec<BweSynthesisWorkspace>,
    tns: Vec<TnsSynthesisWorkspace>,
    fd: Vec<FdShapingWorkspace>,
    synthesis: Vec<Avs3SynthesisWorkspace>,
    spectra: Vec<[f32; BASE_OUTPUT_POSITIONS]>,
    transport_pcm: Vec<[f32; BASE_OUTPUT_POSITIONS]>,
}

impl HoaTransportSynthesisWorkspace {
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
        self.transport_pcm.clear();

        for _ in 0..channel_count {
            self.neural.push(BasicMonoNeuralWorkspace::new());
            self.degroup.push(SpectrumDegroupWorkspace::new());
            self.bwe.push(BweSynthesisWorkspace::new());
            self.tns.push(TnsSynthesisWorkspace::new());
            self.fd.push(FdShapingWorkspace::new());
            self.synthesis.push(Avs3SynthesisWorkspace::new());
            self.spectra.push([0.0; BASE_OUTPUT_POSITIONS]);
            self.transport_pcm.push([0.0; BASE_OUTPUT_POSITIONS]);
        }
    }

    pub fn transport_pcm(&self) -> &[[f32; BASE_OUTPUT_POSITIONS]] {
        &self.transport_pcm
    }

    pub fn spectra(&self) -> &[[f32; BASE_OUTPUT_POSITIONS]] {
        &self.spectra
    }

    pub fn reset(&mut self) {
        for synthesis in &mut self.synthesis {
            synthesis.reset();
        }
        for spectrum in &mut self.spectra {
            spectrum.fill(0.0);
        }
        for pcm in &mut self.transport_pcm {
            pcm.fill(0.0);
        }
    }
}

fn two_spectra_mut(
    spectra: &mut [[f32; BASE_OUTPUT_POSITIONS]],
    first: usize,
    second: usize,
) -> Result<(&mut [f32; BASE_OUTPUT_POSITIONS], &mut [f32; BASE_OUTPUT_POSITIONS]), CodecError> {
    if first == second || first >= spectra.len() || second >= spectra.len() {
        return Err(CodecError::InvalidData("invalid HOA transport-channel pair"));
    }
    if first < second {
        let (before_second, from_second) = spectra.split_at_mut(second);
        Ok((&mut before_second[first], &mut from_second[0]))
    } else {
        let (before_first, from_first) = spectra.split_at_mut(first);
        Ok((&mut from_first[0], &mut before_first[second]))
    }
}

/// Reconstruct the transmitted HOA transport spectra before inverse grouping.
///
/// Each signaled pair applies inverse M/S only to the 21 low-bitrate HOA SFBs selected by its mask
/// (0..768 MDCT lines). After all pair rotations, per-channel HOA ILD gains are applied across the
/// complete 1024-line spectrum. Index 30 is represented as `None` by the side parser and is a no-op.
pub fn apply_inverse_hoa_dmx(
    side: &HoaSideInfo,
    config: &HoaConfig,
    spectra: &mut [[f32; BASE_OUTPUT_POSITIONS]],
) -> Result<(), CodecError> {
    if spectra.len() != usize::from(config.transport_channels)
        || side.groups.len() != config.groups.len()
    {
        return Err(CodecError::InvalidData(
            "HOA inverse DMX transport geometry mismatch",
        ));
    }

    for (group_side, group_config) in side.groups.iter().zip(&config.groups) {
        if group_side.channels != group_config.channels
            || group_side.channel_offset != group_config.channel_offset
        {
            return Err(CodecError::InvalidData(
                "HOA inverse DMX group geometry mismatch",
            ));
        }
        let offset = usize::from(group_config.channel_offset);
        for pair in &group_side.pairs {
            let first = offset + usize::from(pair.first);
            let second = offset + usize::from(pair.second);
            let (first_spectrum, second_spectrum) = two_spectra_mut(spectra, first, second)?;
            for (band, enabled) in pair.sfb_mask.iter().copied().enumerate() {
                if !enabled {
                    continue;
                }
                let start = crate::HOA_SFB_BOUNDARIES[band];
                let stop = crate::HOA_SFB_BOUNDARIES[band + 1];
                for line in start..stop {
                    let original_first = first_spectrum[line];
                    let second_value = second_spectrum[line];
                    first_spectrum[line] =
                        (original_first + second_value) * core::f32::consts::FRAC_1_SQRT_2;
                    second_spectrum[line] =
                        (original_first - second_value) * core::f32::consts::FRAC_1_SQRT_2;
                }
            }
        }
    }

    for (group_side, group_config) in side.groups.iter().zip(&config.groups) {
        if group_side.ild_indices.len() != usize::from(group_config.channels) {
            return Err(CodecError::InvalidData(
                "HOA ILD vector does not match group channel count",
            ));
        }
        let offset = usize::from(group_config.channel_offset);
        for (local_channel, ild) in group_side.ild_indices.iter().copied().enumerate() {
            let Some(index) = ild else {
                continue;
            };
            let factor = mc_ild_factor(index)?;
            for value in &mut spectra[offset + local_channel] {
                *value *= factor;
            }
        }
    }
    Ok(())
}

/// Parse and decode one HOA frame through the complete transport-channel PCM stage.
///
/// Spatial HOA recovery is intentionally a separate stateful stage because the reference profile
/// analyzes the transport PCM with a 512-sample hop and two-frame-delayed basis indices.
pub fn decode_hoa_transport_frame(
    nn_type: NeuralNetworkType,
    payload: &[u8],
    core_bit_offset: usize,
    order: u8,
    total_bitrate_kbps: u32,
    workspace: &mut HoaTransportSynthesisWorkspace,
) -> Result<GaHoaFrameSideInfo, CodecError> {
    let side = parse_hoa_frame_side_info(
        payload,
        core_bit_offset,
        order,
        total_bitrate_kbps,
        nn_type,
    )?;
    let channel_count = usize::from(side.config.transport_channels);
    if side.channels.len() != channel_count
        || side.channel_bwe_configs.len() != channel_count
    {
        return Err(CodecError::Internal(
            "HOA frame parser returned inconsistent transport geometry".into(),
        ));
    }
    workspace.prepare_channels(channel_count);

    for channel_index in 0..channel_count {
        let channel = &side.channels[channel_index];
        let group = side.config.group_for_channel(channel_index)?;
        let noise_fill_lines = if let Some(config) = side.channel_bwe_configs[channel_index] {
            config.target_tiles[0]
                .map(usize::from)
                .ok_or(CodecError::InvalidData(
                    "HOA BWE configuration is missing its start line",
                ))?
        } else {
            usize::from(group.core_lines)
        };
        decode_channel_neural_mdct_with_noise_fill_lines(
            nn_type,
            payload,
            channel,
            noise_fill_lines,
            &mut workspace.neural[channel_index],
            &mut workspace.spectra[channel_index],
        )?;
    }

    apply_inverse_hoa_dmx(&side.hoa, &side.config, &mut workspace.spectra)?;

    let codebooks = normative_lsf_codebooks();
    for channel_index in 0..channel_count {
        let channel = &side.channels[channel_index];
        let spectrum = &mut workspace.spectra[channel_index];
        inverse_group_spectrum(
            channel.core.transform_type,
            channel.group,
            spectrum,
            &mut workspace.degroup[channel_index],
        )?;

        match (side.channel_bwe_configs[channel_index], channel.bwe) {
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
                    "HOA BWE configuration and decoded side information disagree",
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
            &mut workspace.transport_pcm[channel_index],
            &mut workspace.synthesis[channel_index],
        )?;
    }
    Ok(side)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HoaDmxMode, HoaGroupSideInfo, HoaPairSideInfo};

    #[test]
    fn inverse_hoa_dmx_rotates_only_the_normative_low_bitrate_sfb_span() {
        let config = HoaConfig::for_order_bitrate(1, 96).unwrap();
        let mut mask = [true; crate::HOA_SCALE_FACTOR_BANDS];
        mask[0] = true;
        let side = HoaSideInfo {
            scene_type: 0,
            spatial_analysis: false,
            vector_channels: 0,
            basis_indices: Vec::new(),
            groups: vec![HoaGroupSideInfo {
                channels: 4,
                channel_offset: 0,
                pairs: vec![HoaPairSideInfo {
                    pair_index: 0,
                    first: 0,
                    second: 1,
                    mode: HoaDmxMode::FullBand,
                    sfb_mask: mask,
                }],
                ild_indices: vec![None; 4],
                group_bits_ratio: 0,
                channel_bits_ratio: vec![1; 4],
            }],
            next_bit_offset: 0,
        };
        let mut spectra = vec![[0.0_f32; BASE_OUTPUT_POSITIONS]; 4];
        spectra[0][0] = 1.0;
        spectra[1][0] = 1.0;
        spectra[0][800] = 3.0;
        spectra[1][800] = 5.0;

        apply_inverse_hoa_dmx(&side, &config, &mut spectra).unwrap();
        assert!((spectra[0][0] - core::f32::consts::SQRT_2).abs() < 1.0e-6);
        assert!(spectra[1][0].abs() < 1.0e-6);
        assert_eq!(spectra[0][800], 3.0);
        assert_eq!(spectra[1][800], 5.0);
    }

    #[test]
    fn transport_frontend_rejects_truncated_payload_before_neural_decode() {
        let mut workspace = HoaTransportSynthesisWorkspace::new();
        assert!(decode_hoa_transport_frame(
            NeuralNetworkType::Basic,
            &[],
            0,
            1,
            96,
            &mut workspace,
        )
        .is_err());
    }
}
