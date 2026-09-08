use yinqidao_codec_core::CodecError;

use crate::{
    BASE_INPUT_CHANNELS, BASE_INPUT_POSITIONS, BASE_OUTPUT_POSITIONS, BASE_QUANTILE_MEDIANS,
    BitRange, GroupSideInfo, NoiseFillingRng, apply_base_noise_filling_in_place,
    decode_base_latents_into, dequantize_base_latents_into, inverse_scale_in_place,
    low_complexity_feature_scale,
};

const BASE_LATENT_VALUES: usize = BASE_INPUT_POSITIONS * BASE_INPUT_CHANNELS;

/// Reusable scratch for the AVS3 low-complexity inverse-QC path.
///
/// Unlike the Basic neural profile, LC does not run the base decoder CNN after entropy decode.
/// Its 64x16 dequantized latent tensor is already the 1024-line MDCT representation. Keeping the
/// tensor position-major matches the entropy decoder and therefore avoids the channel-major
/// transpose/copy used by the published C reference implementation.
#[derive(Debug, Default)]
pub struct LowComplexityPipelineWorkspace {
    quantized: Vec<i32>,
    dequantized: Vec<f32>,
}

impl LowComplexityPipelineWorkspace {
    pub fn new() -> Self {
        Self::default()
    }

    fn prepare(&mut self) {
        self.quantized.resize(BASE_LATENT_VALUES, 0);
        self.dequantized.resize(BASE_LATENT_VALUES, 0.0);
    }

    pub fn quantized_latents(&self) -> &[i32] {
        &self.quantized
    }

    pub fn dequantized_latents(&self) -> &[f32] {
        &self.dequantized
    }
}

/// Complete low-complexity inverse-QC path from one base range-coded payload to MDCT.
///
/// Normative LC order:
/// `range decode -> inverse quantization -> latent noise filling -> inverse LC feature scale ->
/// direct 64x16 latent-to-1024-MDCT reshape`.
///
/// The final reshape is a zero-transpose copy because this crate already stores dequantized
/// latents in the reference decoder's logical `[position][channel]` output order.
#[allow(clippy::too_many_arguments)]
pub fn decode_low_complexity_base_to_mdct_normative(
    packet: &[u8],
    base_range: BitRange,
    model_indices: &[u8],
    num_lines_noise_fill: usize,
    group: GroupSideInfo,
    nf_param_q_idx: [Option<u8>; 2],
    scale_q_idx_lc: u8,
    rng: &mut NoiseFillingRng,
    workspace: &mut LowComplexityPipelineWorkspace,
    output: &mut [f32],
) -> Result<(), CodecError> {
    if model_indices.len() != BASE_LATENT_VALUES {
        return Err(CodecError::InvalidData(
            "low-complexity base model-index tensor must be sixty-four positions by sixteen channels",
        ));
    }
    if output.len() != BASE_OUTPUT_POSITIONS {
        return Err(CodecError::InvalidData(
            "low-complexity inverse-QC output must contain 1024 MDCT coefficients",
        ));
    }

    workspace.prepare();
    decode_base_latents_into(packet, base_range, model_indices, &mut workspace.quantized)?;
    dequantize_base_latents_into(
        &workspace.quantized,
        &BASE_QUANTILE_MEDIANS,
        &mut workspace.dequantized,
    )?;
    apply_base_noise_filling_in_place(
        &workspace.quantized,
        &mut workspace.dequantized,
        num_lines_noise_fill,
        group,
        nf_param_q_idx,
        rng,
    )?;
    inverse_scale_in_place(
        &mut workspace.dequantized,
        low_complexity_feature_scale(scale_q_idx_lc),
    )?;
    output.copy_from_slice(&workspace.dequantized);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_output_geometry_before_entropy_decode() {
        let group = GroupSideInfo {
            num_groups: 1,
            group_indicator: [false; 8],
            next_bit_offset: 0,
        };
        let models = [0_u8; BASE_LATENT_VALUES];
        let mut rng = NoiseFillingRng::new(1);
        let mut workspace = LowComplexityPipelineWorkspace::new();
        let mut output = [0.0_f32; 8];

        assert!(
            decode_low_complexity_base_to_mdct_normative(
                &[],
                BitRange {
                    bit_offset: 0,
                    bit_len: 0,
                },
                &models,
                0,
                group,
                [Some(0), None],
                255,
                &mut rng,
                &mut workspace,
                &mut output,
            )
            .is_err()
        );
    }

    #[test]
    fn scale_index_255_is_exact_unity() {
        assert_eq!(low_complexity_feature_scale(255), 1.0);
    }
}
