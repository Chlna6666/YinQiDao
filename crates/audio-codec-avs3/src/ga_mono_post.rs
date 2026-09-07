use yinqidao_codec_core::CodecError;

use crate::{
    BASE_OUTPUT_POSITIONS, BasicMonoNeuralWorkspace, BweConfig, BweSynthesisWorkspace,
    GaMonoFrameSideInfo, SpectrumDegroupWorkspace, TnsSynthesisWorkspace, apply_bwe_synthesis,
    apply_inverse_tns, inverse_group_spectrum, parse_and_decode_basic_mono_neural_mdct,
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

/// Parse and decode a Basic-profile mono frame through the complete pre-FD-shaping spectrum path.
///
/// Order:
/// `QC/neural inverse -> spectrum inverse grouping -> BWE -> inverse TNS`.
///
/// The returned 1024-line spectrum is intentionally left before frequency-domain inverse spectrum
/// shaping. That stage requires the normative LSF VQ/codebook path and is kept as a separate
/// milestone instead of being approximated here.
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
