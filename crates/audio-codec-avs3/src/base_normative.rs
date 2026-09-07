use yinqidao_codec_core::CodecError;

use crate::{
    BASE_QUANTILE_MEDIANS, BasePipelineWorkspace, BitRange, GroupSideInfo, NoiseFillingRng,
    base_decoder_params, decode_basic_base_to_mdct,
};

/// Decode the basic-profile base bitstream with the complete built-in AVS3 model parameters.
///
/// B.10..B.23 are selected directly from exact static binary32 tables. The interoperable base
/// scalar quantizer has sixteen exact-zero medians, so callers no longer need to provide a model
/// offset vector. The lower-level [`decode_basic_base_to_mdct`] entry point remains available for
/// tests, differential validation and model experiments.
pub fn decode_basic_base_to_mdct_normative(
    packet: &[u8],
    base_range: BitRange,
    model_indices: &[u8],
    num_lines_noise_fill: usize,
    group: GroupSideInfo,
    nf_param_q_idx: [Option<u8>; 2],
    is_feat_amplified: bool,
    scale_q_idx: u8,
    rng: &mut NoiseFillingRng,
    workspace: &mut BasePipelineWorkspace,
    output: &mut [f32],
) -> Result<(), CodecError> {
    decode_basic_base_to_mdct(
        packet,
        base_range,
        model_indices,
        &BASE_QUANTILE_MEDIANS,
        num_lines_noise_fill,
        group,
        nf_param_q_idx,
        is_feat_amplified,
        scale_q_idx,
        base_decoder_params(),
        rng,
        workspace,
        output,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BASE_INPUT_CHANNELS, BASE_INPUT_POSITIONS};

    #[test]
    fn normative_entrypoint_keeps_geometry_validation() {
        let model_indices = [0_u8; BASE_INPUT_POSITIONS * BASE_INPUT_CHANNELS];
        let group = GroupSideInfo {
            num_groups: 1,
            group_indicator: [false; 8],
            next_bit_offset: 0,
        };
        let mut rng = NoiseFillingRng::new(1);
        let mut workspace = BasePipelineWorkspace::new();
        let mut output = [];

        assert!(decode_basic_base_to_mdct_normative(
            &[],
            BitRange {
                bit_offset: 0,
                bit_len: 0,
            },
            &model_indices,
            0,
            group,
            [Some(0), None],
            false,
            127,
            &mut rng,
            &mut workspace,
            &mut output,
        )
        .is_err());
    }
}
