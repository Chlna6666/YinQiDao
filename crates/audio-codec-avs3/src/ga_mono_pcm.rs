use yinqidao_codec_core::CodecError;

use crate::{
    BweConfig, GaMonoFrameSideInfo, fd_lsf_tables::normative_lsf_codebooks,
    ga_mono_post::{BasicMonoSynthesisWorkspace, parse_decode_basic_mono_pcm as decode_with_codebooks},
};

/// Decode one Basic-profile mono payload to 1024 floating-point PCM samples using the bundled
/// normative Annex-B LSF codebooks.
///
/// The full path is allocation-free after workspace construction:
/// neural inverse-QC -> inverse grouping -> BWE -> inverse TNS -> inverse FD shaping -> IMDCT ->
/// window -> overlap/add.
pub fn parse_decode_basic_mono_pcm(
    payload: &[u8],
    core_bit_offset: usize,
    low_bitrate_precision: bool,
    bwe_config: Option<BweConfig>,
    workspace: &mut BasicMonoSynthesisWorkspace,
    pcm: &mut [f32],
) -> Result<GaMonoFrameSideInfo, CodecError> {
    decode_with_codebooks(
        payload,
        core_bit_offset,
        low_bitrate_precision,
        bwe_config,
        normative_lsf_codebooks(),
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
        assert!(parse_decode_basic_mono_pcm(
            &[],
            0,
            false,
            None,
            &mut workspace,
            &mut pcm,
        )
        .is_err());
    }
}
