use yinqidao_codec_core::CodecError;

use crate::{
    BweConfig, GaMonoFrameSideInfo, NeuralNetworkType,
    fd_lsf_tables::normative_lsf_codebooks,
    ga_mono_post::{
        BasicMonoSynthesisWorkspace, parse_decode_mono_pcm_with_codebooks as decode_with_codebooks,
    },
};

/// Decode one Basic or Low-Complexity mono payload to 1024 floating-point PCM samples using the
/// bundled normative Annex-B LSF codebooks.
pub fn parse_decode_mono_pcm(
    nn_type: NeuralNetworkType,
    payload: &[u8],
    core_bit_offset: usize,
    low_bitrate_precision: bool,
    bwe_config: Option<BweConfig>,
    workspace: &mut BasicMonoSynthesisWorkspace,
    pcm: &mut [f32],
) -> Result<GaMonoFrameSideInfo, CodecError> {
    decode_with_codebooks(
        nn_type,
        payload,
        core_bit_offset,
        low_bitrate_precision,
        bwe_config,
        normative_lsf_codebooks(),
        workspace,
        pcm,
    )
}

/// Compatibility Basic-profile mono entry point.
pub fn parse_decode_basic_mono_pcm(
    payload: &[u8],
    core_bit_offset: usize,
    low_bitrate_precision: bool,
    bwe_config: Option<BweConfig>,
    workspace: &mut BasicMonoSynthesisWorkspace,
    pcm: &mut [f32],
) -> Result<GaMonoFrameSideInfo, CodecError> {
    parse_decode_mono_pcm(
        NeuralNetworkType::Basic,
        payload,
        core_bit_offset,
        low_bitrate_precision,
        bwe_config,
        workspace,
        pcm,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_normative_frontend_checks_pcm_geometry_before_payload_decode() {
        let mut workspace = BasicMonoSynthesisWorkspace::new();
        let mut pcm = [0.0_f32; 8];
        assert!(
            parse_decode_basic_mono_pcm(&[], 0, false, None, &mut workspace, &mut pcm,).is_err()
        );
    }

    #[test]
    fn low_complexity_frontend_uses_same_pcm_geometry_contract() {
        let mut workspace = BasicMonoSynthesisWorkspace::new();
        let mut pcm = [0.0_f32; 8];
        assert!(
            parse_decode_mono_pcm(
                NeuralNetworkType::LowComplexity,
                &[],
                0,
                false,
                None,
                &mut workspace,
                &mut pcm,
            )
            .is_err()
        );
    }
}
